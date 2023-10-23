mod error;

#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    ensure_eq, Addr, CosmosMsg, Deps, DepsMut, Env, Event, MessageInfo, QueryResponse, Response,
    StdResult,
};
use cw_storage_plus::Item;
use error::ContractError;
use hpl_interface::{
    hook::{
        aggregate::{AggregateHookQueryMsg, ExecuteMsg, HooksResponse, InstantiateMsg, QueryMsg},
        post_dispatch, HookQueryMsg, MailboxResponse, PostDispatchMsg, QuoteDispatchResponse,
    },
    to_binary,
    types::{AggregateMetadata, Message},
};
use hpl_ownable::get_owner;

// version info for migration info
pub const CONTRACT_NAME: &str = env!("CARGO_PKG_NAME");
pub const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const HOOKS_KEY: &str = "hooks";
pub const HOOKS: Item<Vec<Addr>> = Item::new(HOOKS_KEY);

fn new_event(name: &str) -> Event {
    Event::new(format!("hpl_hook_aggregate::{}", name))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    cw2::set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let owner = deps.api.addr_validate(&msg.owner)?;
    let hooks = msg
        .hooks
        .iter()
        .map(|v| deps.api.addr_validate(v))
        .collect::<StdResult<_>>()?;

    hpl_ownable::initialize(deps.storage, &owner)?;

    HOOKS.save(deps.storage, &hooks)?;

    Ok(Response::new().add_event(
        new_event("initialize")
            .add_attribute("sender", info.sender)
            .add_attribute("owner", owner)
            .add_attribute("hooks", msg.hooks.join(",")),
    ))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Ownable(msg) => Ok(hpl_ownable::handle(deps, env, info, msg)?),
        ExecuteMsg::PostDispatch(PostDispatchMsg { message, metadata }) => {
            // aggregate it
            let hooks = HOOKS.load(deps.storage)?;

            let msgs = AggregateMetadata::from_hex(metadata, hooks)
                .map(|(hook, metadata)| {
                    post_dispatch(hook, metadata, message.clone(), None).map(|x| x.into())
                })
                .collect::<StdResult<Vec<CosmosMsg>>>()?;

            let decoded_msg: Message = message.into();

            // do nothing
            Ok(Response::new().add_messages(msgs).add_event(
                new_event("post_dispatch").add_attribute("message_id", decoded_msg.id().to_hex()),
            ))
        }
        ExecuteMsg::SetHooks { hooks } => {
            ensure_eq!(
                get_owner(deps.storage)?,
                info.sender,
                ContractError::Unauthorized {}
            );

            let parsed_hooks = hooks
                .iter()
                .map(|v| deps.api.addr_validate(v))
                .collect::<StdResult<_>>()?;

            HOOKS.save(deps.storage, &parsed_hooks)?;

            Ok(Response::new().add_event(
                new_event("set_hooks")
                    .add_attribute("sender", info.sender)
                    .add_attribute("hooks", hooks.join(",")),
            ))
        }
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> Result<QueryResponse, ContractError> {
    match msg {
        QueryMsg::Ownable(msg) => Ok(hpl_ownable::handle_query(deps, env, msg)?),
        QueryMsg::Hook(msg) => match msg {
            HookQueryMsg::Mailbox {} => to_binary(get_mailbox(deps)),
            HookQueryMsg::QuoteDispatch(_) => to_binary(quote_dispatch()),
        },
        QueryMsg::AggregateHook(msg) => match msg {
            AggregateHookQueryMsg::Hooks {} => to_binary(get_hooks(deps)),
        },
    }
}

fn get_mailbox(_deps: Deps) -> Result<MailboxResponse, ContractError> {
    Ok(MailboxResponse {
        mailbox: "unrestricted".to_string(),
    })
}

fn quote_dispatch() -> Result<QuoteDispatchResponse, ContractError> {
    Ok(QuoteDispatchResponse { gas_amount: None })
}

fn get_hooks(deps: Deps) -> Result<HooksResponse, ContractError> {
    Ok(HooksResponse {
        hooks: HOOKS
            .load(deps.storage)?
            .into_iter()
            .map(|v| v.into())
            .collect(),
    })
}
