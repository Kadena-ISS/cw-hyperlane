import {
  CosmWasmClient,
  MsgExecuteContractEncodeObject,
  MsgInstantiateContractEncodeObject,
  MsgStoreCodeEncodeObject,
} from '@cosmjs/cosmwasm-stargate';
import { keccak256 } from '@cosmjs/crypto';
import {
  Coin,
  EncodeObject,
  encodePubkey,
  makeAuthInfoBytes,
} from '@cosmjs/proto-signing';
import { DeliverTxResponse, StargateClient } from '@cosmjs/stargate';
import { TxRaw } from 'cosmjs-types/cosmos/tx/v1beta1/tx';
import { readFileSync } from 'fs';

import { waitTx } from '../../script/shared/utils';
import { denom, reg } from './deps';
import { migrationTargets } from './migration';
import { QUERIES } from './queries';
import { ClientSet, Member } from './types';

export async function makeMember(
  client: ClientSet,
  address: string,
): Promise<Member> {
  const account = await client.stargate.getAccount(address);
  if (!account?.pubkey) throw new Error(`Account ${address} has no pubkey`);

  return {
    address,
    pubkey: encodePubkey({
      type: 'tendermint/PubKeySecp256k1',
      value: account.pubkey.value,
    }),
    client,
  };
}

export async function initContract(
  member: Member,
  codeId: bigint,
  msg: object,
  label: string = 'contract',
) {
  const resp = await sendTx(member, [
    {
      typeUrl: '/cosmwasm.wasm.v1.MsgInstantiateContract',
      value: {
        sender: member.address,
        codeId,
        msg: Buffer.from(JSON.stringify(msg)),
        label,
      },
    } as MsgInstantiateContractEncodeObject,
  ]);

  const waitResp = await waitTx(resp.transactionHash, member.client.stargate);
  if (waitResp.code !== 0) throw new Error(`Tx failed: ${waitResp.rawLog}`);
  return waitResp;
}

export async function uploadContract(member: Member, contractPath: string) {
  const resp = await sendTx(member, [
    {
      typeUrl: '/cosmwasm.wasm.v1.MsgStoreCode',
      value: {
        sender: member.address,
        wasmByteCode: readFileSync(contractPath),
      },
    } as MsgStoreCodeEncodeObject,
  ]);

  const waitResp = await waitTx(resp.transactionHash, member.client.stargate);
  if (waitResp.code !== 0) throw new Error(`Tx failed: ${waitResp.rawLog}`);
  return waitResp;
}

export async function executeContract(
  member: Member,
  contract: string,
  msg: object,
  funds?: Coin[],
) {
  const resp = await sendTx(member, [
    {
      typeUrl: '/cosmwasm.wasm.v1.MsgExecuteContract',
      value: {
        sender: member.address,
        contract: contract,
        msg: Buffer.from(JSON.stringify(msg)),
        funds,
      },
    } as MsgExecuteContractEncodeObject,
  ]);

  const waitResp = await waitTx(resp.transactionHash, member.client.stargate);
  if (waitResp.code !== 0) throw new Error(`Tx failed: ${waitResp.rawLog}`);
  return waitResp;
}

export async function sendTx(
  from: Member,
  messages: EncodeObject[],
): Promise<DeliverTxResponse> {
  const txBodyFields = {
    typeUrl: '/cosmos.tx.v1beta1.TxBody',
    value: { messages },
  };

  const feeAmount = [
    {
      amount: `${1_000_000}`,
      denom,
    },
  ];

  const txBodyBytes = reg.encode(txBodyFields);

  if (!from.pubkey) from = await makeMember(from.client, from.address);

  const gasLimit = 2_000_000;
  const authInfoBytes = makeAuthInfoBytes(
    [{ pubkey: from.pubkey!, sequence: 0 }],
    feeAmount,
    gasLimit,
    undefined,
    undefined,
  );

  const txRawBytes = Uint8Array.from(
    TxRaw.encode(
      TxRaw.fromPartial({
        bodyBytes: txBodyBytes,
        authInfoBytes: authInfoBytes,
        signatures: [],
      }),
    ).finish(),
  );

  return from.client.stargate.broadcastTx(txRawBytes);
}

export type Snapshot = {
  contract: string;
  address: string;
  results: {
    id: string;
    query: object;
    response?: object;
    error?: unknown;
  }[];
}[];

export async function makeSnapshot(client: {
  wasm: CosmWasmClient;
  stargate: StargateClient;
}): Promise<Snapshot> {
  const snapshot = [];

  console.log('Generating snapshot...');

  for (const contract of migrationTargets) {
    const found = QUERIES.find((v) => v.contract === contract.name);
    if (!found) throw new Error(`No queries found for ${contract.name}`);

    console.log(`Processing ${contract.name}...`);

    for (const target of contract.address) {
      console.log(`=> CONTRACT: ${target}`);

      const results: Snapshot[0]['results'] = [];

      for (const query of found.queries) {
        console.log(`==> QUERING: ${JSON.stringify(query)}`);

        const queryIdRaw = keccak256(Buffer.from(JSON.stringify(query)));
        const queryId = Buffer.from(queryIdRaw).toString('hex');

        try {
          const resp = await client.wasm.queryContractSmart(target, query);
          console.log(JSON.stringify(resp));

          results.push({
            id: queryId,
            query,
            response: resp,
          });
        } catch (e) {
          console.error(e);
          results.push({ id: queryId, query, error: e });
        }
      }

      snapshot.push({
        contract: contract.name,
        address: target,
        results,
      });
    }
  }

  return snapshot;
}
