#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    ensure_eq, from_binary, CosmosMsg, Deps, DepsMut, Env, HexBinary, MessageInfo, QueryResponse,
    Reply, Response, SubMsg, Uint256, WasmMsg,
};

use cw20::Cw20ReceiveMsg;
use hpl_interface::{
    core::mailbox,
    to_binary,
    types::bech32_encode,
    warp::{
        self,
        cw20::{ExecuteMsg, InstantiateMsg, QueryMsg, ReceiveMsg},
        TokenMode, TokenModeMsg, TokenModeResponse, TokenTypeResponse,
    },
};
use hpl_router::get_route;

use crate::{
    conv, error::ContractError, new_event, CONTRACT_NAME, CONTRACT_VERSION, HRP, MAILBOX, MODE,
    REPLY_ID_CREATE_DENOM, TOKEN,
};

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    cw2::set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let mode: TokenMode = msg.token.clone().into();
    let owner = deps.api.addr_validate(&msg.owner)?;
    let mailbox = deps.api.addr_validate(&msg.mailbox)?;

    HRP.save(deps.storage, &msg.hrp)?;
    MODE.save(deps.storage, &mode)?;
    MAILBOX.save(deps.storage, &mailbox)?;

    hpl_ownable::initialize(deps.storage, &owner)?;

    let mut denom = "".into();

    let msgs = match msg.token {
        TokenModeMsg::Bridged(token) => {
            vec![SubMsg::reply_on_success(
                WasmMsg::Instantiate {
                    admin: Some(env.contract.address.to_string()),
                    code_id: token.code_id,
                    msg: cosmwasm_std::to_binary(&token.init_msg)?,
                    funds: vec![],
                    label: "token warp cw20".to_string(),
                },
                REPLY_ID_CREATE_DENOM,
            )]
        }
        TokenModeMsg::Collateral(token) => {
            let token_addr = deps.api.addr_validate(&token.address)?;
            TOKEN.save(deps.storage, &token_addr)?;
            denom = token_addr.to_string();
            vec![]
        }
    };

    Ok(Response::new().add_submessages(msgs).add_event(
        new_event("instantiate")
            .add_attribute("sender", info.sender)
            .add_attribute("owner", owner)
            .add_attribute("mode", format!("{mode}"))
            .add_attribute("denom", denom),
    ))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    use ExecuteMsg::*;

    match msg {
        Router(msg) => Ok(hpl_router::handle(deps, env, info, msg)?),
        Handle(msg) => mailbox_handle(deps, info, msg),
        Receive(msg) => {
            ensure_eq!(
                info.sender,
                TOKEN.load(deps.storage)?,
                ContractError::Unauthorized
            );

            match from_binary::<ReceiveMsg>(&msg.msg)? {
                ReceiveMsg::TransferRemote {
                    dest_domain,
                    recipient,
                } => transfer_remote(deps, msg, dest_domain, recipient),
            }
        }
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, _env: Env, msg: Reply) -> Result<Response, ContractError> {
    match msg.id {
        REPLY_ID_CREATE_DENOM => {
            let reply_data = msg.result.unwrap().data.unwrap();
            let init_resp = cw_utils::parse_instantiate_response_data(&reply_data)?;
            let init_addr = deps.api.addr_validate(&init_resp.contract_address)?;

            TOKEN.save(deps.storage, &init_addr)?;

            let resp = Response::new()
                .add_event(new_event("reply-init").add_attribute("new_token", init_addr));

            Ok(resp)
        }

        _ => Err(ContractError::InvalidReplyId),
    }
}

fn mailbox_handle(
    deps: DepsMut,
    info: MessageInfo,
    msg: hpl_interface::core::HandleMsg,
) -> Result<Response, ContractError> {
    // validate mailbox
    ensure_eq!(
        info.sender,
        MAILBOX.load(deps.storage)?,
        ContractError::Unauthorized
    );
    // validate origin chain router
    ensure_eq!(
        msg.sender,
        get_route::<HexBinary>(deps.storage, msg.origin)?
            .route
            .expect("route not found"),
        ContractError::Unauthorized
    );

    let token_msg: warp::Message = msg.body.into();
    let recipient = bech32_encode(&HRP.load(deps.storage)?, &token_msg.recipient)?;

    let token = TOKEN.load(deps.storage)?;
    let mode = MODE.load(deps.storage)?;

    let msg = match mode {
        // make token mint msg if token mode is bridged
        TokenMode::Bridged => conv::to_mint_msg(&token, &recipient, token_msg.amount)?,
        // make token transfer msg if token mode is collateral
        // we can consider to use MsgSend for further utility
        TokenMode::Collateral => conv::to_send_msg(&token, &recipient, token_msg.amount)?,
    };

    Ok(Response::new().add_message(msg).add_event(
        new_event("handle")
            .add_attribute("recipient", recipient)
            .add_attribute("token", token)
            .add_attribute("amount", token_msg.amount),
    ))
}

fn transfer_remote(
    deps: DepsMut,
    receive_msg: Cw20ReceiveMsg,
    dest_domain: u32,
    recipient: HexBinary,
) -> Result<Response, ContractError> {
    let token = TOKEN.load(deps.storage)?;
    let mode = MODE.load(deps.storage)?;
    let mailbox = MAILBOX.load(deps.storage)?;

    let dest_router = get_route::<HexBinary>(deps.storage, dest_domain)?
        .route
        .expect("route not found");

    let mut msgs: Vec<CosmosMsg> = vec![];

    if mode == TokenMode::Bridged {
        // push token burn msg if token is bridged
        msgs.push(conv::to_burn_msg(&token, receive_msg.amount)?.into());
    }

    // push mailbox dispatch msg
    msgs.push(mailbox::dispatch(
        mailbox,
        dest_domain,
        dest_router,
        warp::Message {
            recipient: recipient.clone(),
            amount: Uint256::from_uint128(receive_msg.amount),
            metadata: HexBinary::default(),
        }
        .into(),
        None,
        None,
    )?);

    Ok(Response::new().add_messages(msgs).add_event(
        new_event("transfer-remote")
            .add_attribute("sender", receive_msg.sender)
            .add_attribute("dest_domain", dest_domain.to_string())
            .add_attribute("recipient", recipient.to_hex())
            .add_attribute("token", token)
            .add_attribute("amount", receive_msg.amount),
    ))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> Result<QueryResponse, ContractError> {
    use warp::TokenWarpDefaultQueryMsg::*;

    match msg {
        QueryMsg::Ownable(msg) => Ok(hpl_ownable::handle_query(deps, env, msg)?),
        QueryMsg::Router(msg) => Ok(hpl_router::handle_query(deps, env, msg)?),
        QueryMsg::TokenDefault(msg) => match msg {
            TokenType {} => to_binary(get_token_type(deps)),
            TokenMode {} => to_binary(get_token_mode(deps)),
        },
    }
}

fn get_token_type(deps: Deps) -> Result<TokenTypeResponse, ContractError> {
    let contract = TOKEN.load(deps.storage)?.into_string();

    Ok(TokenTypeResponse {
        typ: warp::TokenType::CW20 { contract },
    })
}

fn get_token_mode(deps: Deps) -> Result<TokenModeResponse, ContractError> {
    let mode = MODE.load(deps.storage)?;

    Ok(TokenModeResponse { mode })
}

#[cfg(test)]
mod test {
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};
    use hpl_interface::warp::cw20::{Cw20ModeBridged, Cw20ModeCollateral};
    use rstest::{fixture, rstest};

    use super::*;

    const DEPLOYER: &str = "sender";
    const OWNER: &str = "owner";
    const MAILBOX: &str = "mailbox";

    const CW20_BRIDGED_CODE_ID: u64 = 1;
    const CW20_BRIDGED_NAME: &str = "cw20-created";
    const CW20_COLLATERAL_ADDRESS: &str = "cw20-exisiting";

    type Cw20TokenMode = TokenModeMsg<Cw20ModeBridged, Cw20ModeCollateral>;

    #[fixture]
    fn token_mode_bridge() -> Cw20TokenMode {
        TokenModeMsg::Bridged(Cw20ModeBridged {
            code_id: CW20_BRIDGED_CODE_ID,
            init_msg: cw20_base::msg::InstantiateMsg {
                name: CW20_BRIDGED_NAME.to_string(),
                symbol: CW20_BRIDGED_NAME.to_string(),
                decimals: 1,
                initial_balances: vec![],
                mint: None,
                marketing: None,
            }
            .into(),
        })
    }

    #[fixture]
    fn token_mode_collateral() -> Cw20TokenMode {
        TokenModeMsg::Collateral(Cw20ModeCollateral {
            address: CW20_COLLATERAL_ADDRESS.to_string(),
        })
    }

    #[rstest]
    #[case(token_mode_bridge())]
    #[case(token_mode_collateral())]
    fn test_init(#[values("osmo", "neutron")] hrp: &str, #[case] token_mode: Cw20TokenMode) {
        let mut deps = mock_dependencies();

        let mode = token_mode.clone().into();

        let res = instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info(DEPLOYER, &[]),
            InstantiateMsg {
                token: token_mode.clone(),
                hrp: hrp.to_string(),
                owner: OWNER.to_string(),
                mailbox: MAILBOX.to_string(),
            },
        )
        .unwrap();

        let storage = deps.as_ref().storage;
        assert_eq!(super::HRP.load(storage).unwrap(), hrp);
        assert_eq!(super::MODE.load(storage).unwrap(), mode);
        assert_eq!(super::MAILBOX.load(storage).unwrap(), MAILBOX);

        match token_mode {
            TokenModeMsg::Bridged(v) => {
                assert!(!super::TOKEN.exists(storage));

                let reply = res.messages.get(0).unwrap();
                assert_eq!(reply.id, REPLY_ID_CREATE_DENOM);
                assert_eq!(
                    reply.msg,
                    CosmosMsg::Wasm(WasmMsg::Instantiate {
                        admin: Some(mock_env().contract.address.to_string()),
                        code_id: v.code_id,
                        msg: cosmwasm_std::to_binary(&v.init_msg).unwrap(),
                        funds: vec![],
                        label: "token warp cw20".to_string()
                    })
                )
            }
            TokenModeMsg::Collateral(v) => {
                assert_eq!(super::TOKEN.load(storage).unwrap(), v.address);
                assert!(res.messages.is_empty())
            }
        }
    }
}
