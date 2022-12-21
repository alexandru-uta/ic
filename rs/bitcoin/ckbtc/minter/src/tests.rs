use crate::{
    address::BitcoinAddress, build_unsigned_transaction, fake_sign, greedy,
    signature::EncodedSignature, tx, BuildTxError,
};
use crate::{
    lifecycle::init::InitArgs,
    state::{ChangeOutput, CkBtcMinterState, RetrieveBtcRequest, RetrieveBtcStatus},
};
use bitcoin::network::constants::Network as BtcNetwork;
use bitcoin::util::psbt::serialize::{Deserialize, Serialize};
use ic_base_types::{CanisterId, PrincipalId};
use ic_btc_types::{Network, OutPoint, Satoshi, Utxo};
use ic_icrc1::Account;
use proptest::proptest;
use proptest::{
    array::uniform20,
    array::uniform32,
    collection::{btree_set, vec as pvec, SizeRange},
    option,
    prelude::{any, Strategy},
};
use proptest::{prop_assert, prop_assert_eq, prop_assume, prop_oneof};
use serde_bytes::ByteBuf;
use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;

fn dummy_utxo_from_value(v: u64) -> Utxo {
    Utxo {
        outpoint: OutPoint {
            txid: v.to_be_bytes().to_vec(),
            vout: 0,
        },
        value: v,
        height: 0,
    }
}

fn address_to_script_pubkey(address: &BitcoinAddress) -> bitcoin::Script {
    let address_string = address.display(Network::Mainnet);
    let btc_address = bitcoin::Address::from_str(&address_string).unwrap();
    btc_address.script_pubkey()
}

fn network_to_btc_network(network: Network) -> BtcNetwork {
    match network {
        Network::Mainnet => BtcNetwork::Bitcoin,
        Network::Testnet => BtcNetwork::Testnet,
        Network::Regtest => BtcNetwork::Regtest,
    }
}

fn address_to_btc_address(address: &BitcoinAddress, network: Network) -> bitcoin::Address {
    use bitcoin::util::address::{Payload, WitnessVersion};
    match address {
        BitcoinAddress::P2wpkhV0(pkhash) => bitcoin::Address {
            payload: Payload::WitnessProgram {
                version: WitnessVersion::V0,
                program: pkhash.to_vec(),
            },
            network: network_to_btc_network(network),
        },
        BitcoinAddress::P2pkh(pkhash) => bitcoin::Address {
            payload: Payload::PubkeyHash(bitcoin::PubkeyHash::from_hash(
                bitcoin::hashes::Hash::from_slice(pkhash).unwrap(),
            )),
            network: network_to_btc_network(network),
        },
        BitcoinAddress::P2sh(script_hash) => bitcoin::Address {
            payload: Payload::ScriptHash(bitcoin::ScriptHash::from_hash(
                bitcoin::hashes::Hash::from_slice(script_hash).unwrap(),
            )),
            network: network_to_btc_network(network),
        },
    }
}

fn as_txid(hash: &[u8]) -> bitcoin::Txid {
    bitcoin::Txid::from_hash(bitcoin::hashes::Hash::from_slice(hash).unwrap())
}

fn p2wpkh_script_code(pkhash: &[u8; 20]) -> bitcoin::Script {
    use bitcoin::blockdata::{opcodes, script::Builder};

    Builder::new()
        .push_opcode(opcodes::all::OP_DUP)
        .push_opcode(opcodes::all::OP_HASH160)
        .push_slice(&pkhash[..])
        .push_opcode(opcodes::all::OP_EQUALVERIFY)
        .push_opcode(opcodes::all::OP_CHECKSIG)
        .into_script()
}

fn unsigned_tx_to_bitcoin_tx(tx: &tx::UnsignedTransaction) -> bitcoin::Transaction {
    bitcoin::Transaction {
        version: tx::TX_VERSION as i32,
        lock_time: tx.lock_time,
        input: tx
            .inputs
            .iter()
            .map(|txin| bitcoin::TxIn {
                previous_output: bitcoin::OutPoint {
                    txid: as_txid(&txin.previous_output.txid),
                    vout: txin.previous_output.vout,
                },
                sequence: txin.sequence,
                script_sig: bitcoin::Script::default(),
                witness: bitcoin::Witness::default(),
            })
            .collect(),
        output: tx
            .outputs
            .iter()
            .map(|txout| bitcoin::TxOut {
                value: txout.value,
                script_pubkey: address_to_script_pubkey(&txout.address),
            })
            .collect(),
    }
}

fn signed_tx_to_bitcoin_tx(tx: &tx::SignedTransaction) -> bitcoin::Transaction {
    bitcoin::Transaction {
        version: tx::TX_VERSION as i32,
        lock_time: tx.lock_time,
        input: tx
            .inputs
            .iter()
            .map(|txin| bitcoin::TxIn {
                previous_output: bitcoin::OutPoint {
                    txid: as_txid(&txin.previous_output.txid),
                    vout: txin.previous_output.vout,
                },
                sequence: txin.sequence,
                script_sig: bitcoin::Script::default(),
                witness: bitcoin::Witness::from_vec(vec![
                    txin.signature.as_slice().to_vec(),
                    txin.pubkey.to_vec(),
                ]),
            })
            .collect(),
        output: tx
            .outputs
            .iter()
            .map(|txout| bitcoin::TxOut {
                value: txout.value,
                script_pubkey: address_to_script_pubkey(&txout.address),
            })
            .collect(),
    }
}

#[test]
fn greedy_smoke_test() {
    let mut utxos: BTreeSet<Utxo> = (1..10u64).map(dummy_utxo_from_value).collect();
    assert_eq!(utxos.len(), 9_usize);

    let res = greedy(15, &mut utxos);

    assert_eq!(res[0].value, 9_u64);
    assert_eq!(res[1].value, 6_u64);
}

#[test]
fn test_min_change_amount() {
    let mut available_utxos = BTreeSet::new();
    available_utxos.insert(Utxo {
        outpoint: OutPoint {
            txid: vec![0; 32],
            vout: 0,
        },
        value: 100_000,
        height: 10,
    });

    available_utxos.insert(Utxo {
        outpoint: OutPoint {
            txid: vec![1; 32],
            vout: 1,
        },
        value: 100_000,
        height: 10,
    });

    let minter_addr = BitcoinAddress::P2wpkhV0([0; 20]);
    let out1_addr = BitcoinAddress::P2wpkhV0([1; 20]);
    let out2_addr = BitcoinAddress::P2wpkhV0([2; 20]);
    let fee_per_vbyte = 10000;

    let (tx, change_output, _) = build_unsigned_transaction(
        &mut available_utxos,
        vec![(out1_addr.clone(), 100_000), (out2_addr.clone(), 99_999)],
        minter_addr.clone(),
        fee_per_vbyte,
    )
    .expect("failed to build a transaction");

    let fee = fake_sign(&tx).vsize() as u64 * fee_per_vbyte / 1000;

    assert_eq!(tx.outputs.len(), 3);
    let fee_share = (fee + crate::MIN_CHANGE - 1) / 2;
    assert_eq!(
        &tx.outputs,
        &[
            tx::TxOut {
                address: out1_addr,
                value: 100_000 - fee_share,
            },
            tx::TxOut {
                address: out2_addr,
                value: 99_999 - fee_share,
            },
            tx::TxOut {
                address: minter_addr,
                value: crate::MIN_CHANGE,
            }
        ]
    );
    assert_eq!(
        change_output,
        Some(ChangeOutput {
            vout: 2,
            value: crate::MIN_CHANGE,
        })
    );
}

#[test]
fn test_no_zero_outputs() {
    let mut available_utxos = BTreeSet::new();
    available_utxos.insert(Utxo {
        outpoint: OutPoint {
            txid: vec![0; 32],
            vout: 0,
        },
        value: 100_000,
        height: 10,
    });

    let minter_addr = BitcoinAddress::P2wpkhV0([0; 20]);
    let out1_addr = BitcoinAddress::P2wpkhV0([1; 20]);
    let out2_addr = BitcoinAddress::P2wpkhV0([2; 20]);
    let fee_per_vbyte = 10000;

    assert_eq!(
        build_unsigned_transaction(
            &mut available_utxos,
            vec![(out1_addr, 99_900), (out2_addr.clone(), 100)],
            minter_addr,
            fee_per_vbyte,
        ),
        Err(BuildTxError::ZeroOutput {
            address: out2_addr,
            amount: 100
        })
    );

    assert_eq!(available_utxos.len(), 1);
}

fn arb_amount() -> impl Strategy<Value = Satoshi> {
    1..10_000_000_000u64
}

fn arb_out_point() -> impl Strategy<Value = tx::OutPoint> {
    (pvec(any::<u8>(), 32), any::<u32>()).prop_map(|(txid, vout)| tx::OutPoint { txid, vout })
}

fn arb_unsigned_input(
    value: impl Strategy<Value = Satoshi>,
) -> impl Strategy<Value = tx::UnsignedInput> {
    (arb_out_point(), value, any::<u32>()).prop_map(|(previous_output, value, sequence)| {
        tx::UnsignedInput {
            previous_output,
            value,
            sequence,
        }
    })
}

fn arb_signed_input() -> impl Strategy<Value = tx::SignedInput> {
    (
        arb_out_point(),
        any::<u32>(),
        pvec(1u8..0xff, 64),
        pvec(any::<u8>(), 32),
    )
        .prop_map(
            |(previous_output, sequence, sec1, pubkey)| tx::SignedInput {
                previous_output,
                sequence,
                signature: EncodedSignature::from_sec1(&sec1),
                pubkey: ByteBuf::from(pubkey),
            },
        )
}

fn arb_address() -> impl Strategy<Value = BitcoinAddress> {
    prop_oneof![
        uniform20(any::<u8>()).prop_map(BitcoinAddress::P2wpkhV0),
        uniform20(any::<u8>()).prop_map(BitcoinAddress::P2pkh),
        uniform20(any::<u8>()).prop_map(BitcoinAddress::P2sh),
    ]
}

fn arb_tx_out() -> impl Strategy<Value = tx::TxOut> {
    (arb_amount(), arb_address()).prop_map(|(value, address)| tx::TxOut { value, address })
}

fn arb_utxo(amount: impl Strategy<Value = Satoshi>) -> impl Strategy<Value = Utxo> {
    (amount, pvec(any::<u8>(), 32), 0..5u32).prop_map(|(value, txid, vout)| Utxo {
        outpoint: OutPoint { txid, vout },
        value,
        height: 0,
    })
}

fn arb_account() -> impl Strategy<Value = Account> {
    (pvec(any::<u8>(), 32), option::of(uniform32(any::<u8>()))).prop_map(|(pk, subaccount)| {
        Account {
            owner: PrincipalId::new_self_authenticating(&pk),
            subaccount,
        }
    })
}

fn arb_retrieve_btc_requests(
    amount: impl Strategy<Value = Satoshi>,
    num: impl Into<SizeRange>,
) -> impl Strategy<Value = Vec<RetrieveBtcRequest>> {
    let request_strategy = (
        amount,
        arb_address(),
        any::<u64>(),
        1569975147000..2069975147000u64,
    )
        .prop_map(
            |(amount, address, block_index, received_at)| RetrieveBtcRequest {
                amount,
                address,
                block_index,
                received_at,
            },
        );
    pvec(request_strategy, num).prop_map(|mut reqs| {
        reqs.sort_by_key(|req| req.received_at);

        for (i, req) in reqs.iter_mut().enumerate() {
            req.block_index = i as u64;
        }

        reqs
    })
}

proptest! {
    #[test]
    fn greedy_solution_properties(
        values in pvec(1u64..1_000_000_000, 1..10),
        target in 1u64..1_000_000_000,
    ) {
        let mut utxos: BTreeSet<Utxo> = values
            .into_iter()
            .map(dummy_utxo_from_value)
            .collect();

        let total = utxos.iter().map(|u| u.value).sum::<u64>();

        if total < target {
            utxos.insert(dummy_utxo_from_value(target - total));
        }

        let original_utxos = utxos.clone();

        let solution = greedy(target, &mut utxos);

        prop_assert!(
            !solution.is_empty(),
            "greedy() must always find a solution given enough available UTXOs"
        );

        prop_assert!(
            solution.iter().map(|u| u.value).sum::<u64>() >= target,
            "greedy() must reach the specified target amount"
        );

        prop_assert!(
            solution.iter().all(|u| original_utxos.contains(u)),
            "greedy() must select utxos from the available set"
        );

        prop_assert!(
            solution.iter().all(|u| !utxos.contains(u)),
            "greedy() must remove found UTXOs from the available set"
        );
    }

    #[test]
    fn greedy_does_not_modify_input_when_fails(
        values in pvec(1u64..1_000_000_000, 1..10),
    ) {
        let mut utxos: BTreeSet<Utxo> = values
            .into_iter()
            .map(dummy_utxo_from_value)
            .collect();

        let total = utxos.iter().map(|u| u.value).sum::<u64>();

        let original_utxos = utxos.clone();
        let solution = greedy(total + 1, &mut utxos);

        prop_assert!(solution.is_empty());
        prop_assert_eq!(utxos, original_utxos);
    }

    #[test]
    fn unsigned_tx_encoding_model(
        inputs in pvec(arb_unsigned_input(5_000u64..1_000_000_000), 1..20),
        outputs in pvec(arb_tx_out(), 1..20),
        lock_time in any::<u32>(),
    ) {
        let arb_tx = tx::UnsignedTransaction { inputs, outputs, lock_time };
        println!("{:?}", arb_tx);
        let btc_tx = unsigned_tx_to_bitcoin_tx(&arb_tx);
        println!("{:?}", btc_tx.serialize());

        let tx_bytes = tx::encode_into(&arb_tx, Vec::<u8>::new());
        println!("{:?}", tx_bytes);
        let decoded_btc_tx = bitcoin::Transaction::deserialize(&tx_bytes).expect("failed to deserialize an unsigned transaction");

        prop_assert_eq!(btc_tx.serialize(), tx_bytes);
        prop_assert_eq!(&decoded_btc_tx, &btc_tx);
        prop_assert_eq!(&arb_tx.txid(), &*btc_tx.txid());
    }

    #[test]
    fn unsigned_tx_sighash_model(
        inputs_data in pvec(
            (
                arb_utxo(5_000u64..1_000_000_000),
                any::<u32>(),
                pvec(any::<u8>(), tx::PUBKEY_LEN)
            ),
            1..20
        ),
        outputs in pvec(arb_tx_out(), 1..20),
        lock_time in any::<u32>(),
    ) {
        let inputs: Vec<tx::UnsignedInput> = inputs_data
            .iter()
            .map(|(utxo, seq, _)| tx::UnsignedInput {
                previous_output: utxo.outpoint.clone(),
                value: utxo.value,
                sequence: *seq,
            })
            .collect();
        let arb_tx = tx::UnsignedTransaction { inputs, outputs, lock_time };
        let btc_tx = unsigned_tx_to_bitcoin_tx(&arb_tx);

        let sighasher = tx::TxSigHasher::new(&arb_tx);
        let mut btc_sighasher = bitcoin::util::sighash::SighashCache::new(&btc_tx);

        for (i, (utxo, _, pubkey)) in inputs_data.iter().enumerate() {
            let mut buf = Vec::<u8>::new();
            let pkhash = tx::hash160(pubkey);

            sighasher.encode_sighash_data(i, &pkhash, &mut buf);

            let mut btc_buf = Vec::<u8>::new();
            let script_code = p2wpkh_script_code(&pkhash);
            btc_sighasher.segwit_encode_signing_data_to(&mut btc_buf, i, &script_code, utxo.value, bitcoin::EcdsaSighashType::All)
                .expect("failed to encode sighash data");
            prop_assert_eq!(hex::encode(&buf), hex::encode(&btc_buf));

            let sighash = sighasher.sighash(i, &pkhash);
            let btc_sighash = btc_sighasher.segwit_signature_hash(i, &script_code, utxo.value, bitcoin::EcdsaSighashType::All).unwrap();
            prop_assert_eq!(hex::encode(sighash), hex::encode(btc_sighash));
        }
    }

    #[test]
    fn signed_tx_encoding_model(
        inputs in pvec(arb_signed_input(), 1..20),
        outputs in pvec(arb_tx_out(), 1..20),
        lock_time in any::<u32>(),
    ) {
        let arb_tx = tx::SignedTransaction { inputs, outputs, lock_time };
        println!("{:?}", arb_tx);
        let btc_tx = signed_tx_to_bitcoin_tx(&arb_tx);
        println!("{:?}", btc_tx.serialize());

        let tx_bytes = tx::encode_into(&arb_tx, Vec::<u8>::new());
        println!("{:?}", tx_bytes);
        let decoded_btc_tx = bitcoin::Transaction::deserialize(&tx_bytes).expect("failed to deserialize a signed transaction");

        prop_assert_eq!(btc_tx.serialize(), tx_bytes);
        prop_assert_eq!(&decoded_btc_tx, &btc_tx);
        prop_assert_eq!(&arb_tx.wtxid(), &*btc_tx.wtxid());
        prop_assert_eq!(arb_tx.vsize(), btc_tx.vsize());
    }

    #[test]
    fn build_tx_splits_utxos(
        mut utxos in btree_set(arb_utxo(5_000u64..1_000_000_000), 1..20),
        dst_pkhash in uniform20(any::<u8>()),
        main_pkhash in uniform20(any::<u8>()),
        fee_per_vbyte in 1000..2000u64,
    ) {
        let value_by_outpoint: HashMap<_, _> = utxos
            .iter()
            .map(|utxo| (utxo.outpoint.clone(), utxo.value))
            .collect();

        let utxo_count = utxos.len();
        let total_value = utxos.iter().map(|u| u.value).sum::<u64>();

        let target = total_value / 2;
        let (unsigned_tx, _, _) = build_unsigned_transaction(
            &mut utxos,
            vec![(BitcoinAddress::P2wpkhV0(dst_pkhash), target)],
            BitcoinAddress::P2wpkhV0(main_pkhash),
            fee_per_vbyte
        )
        .expect("failed to build transaction");

        let fee = fake_sign(&unsigned_tx).vsize() as u64 * fee_per_vbyte / 1000;

        let inputs_value = unsigned_tx.inputs
            .iter()
            .map(|input| value_by_outpoint.get(&input.previous_output).unwrap())
            .sum::<u64>();

        prop_assert!(inputs_value >= target);
        prop_assert!(fee < target);
        prop_assert_eq!(utxo_count, unsigned_tx.inputs.len() + utxos.len());
        prop_assert_eq!(utxos.iter().map(|u| u.value).sum::<u64>(), total_value - inputs_value);
    }

    #[test]
    fn build_tx_handles_change_from_inputs(
        mut utxos in btree_set(arb_utxo(1_000_000u64..1_000_000_000), 1..20),
        dst_pkhash in uniform20(any::<u8>()),
        main_pkhash in uniform20(any::<u8>()),
        target in 10000..50000u64,
        fee_per_vbyte in 1000..2000u64,
    ) {
        prop_assume!(dst_pkhash != main_pkhash);

        let value_by_outpoint: HashMap<_, _> = utxos
            .iter()
            .map(|utxo| (utxo.outpoint.clone(), utxo.value))
            .collect();

        let (unsigned_tx, change_output, _) = build_unsigned_transaction(
            &mut utxos,
            vec![(BitcoinAddress::P2wpkhV0(dst_pkhash), target)],
            BitcoinAddress::P2wpkhV0(main_pkhash),
            fee_per_vbyte
        )
        .expect("failed to build transaction");

        let fee = fake_sign(&unsigned_tx).vsize() as u64 * fee_per_vbyte / 1000;

        let inputs_value = unsigned_tx.inputs
            .iter()
            .map(|input| value_by_outpoint.get(&input.previous_output).unwrap())
            .sum::<u64>();

        prop_assert_eq!(
            &unsigned_tx.outputs,
            &vec![
                tx::TxOut {
                    value: target - fee,
                    address: BitcoinAddress::P2wpkhV0(dst_pkhash),
                },
                tx::TxOut {
                    value: inputs_value - target,
                    address: BitcoinAddress::P2wpkhV0(main_pkhash),
                },
            ]
        );

        prop_assert_eq!(change_output, Some(ChangeOutput { vout: 1, value: inputs_value - target }));
    }

    #[test]
    fn build_tx_does_not_modify_utxos_on_error(
        mut utxos in btree_set(arb_utxo(5_000u64..1_000_000_000), 1..20),
        dst_pkhash in uniform20(any::<u8>()),
        main_pkhash in uniform20(any::<u8>()),
        fee_per_vbyte in 1000..2000u64,
    ) {
        let utxos_copy = utxos.clone();

        let total_value = utxos.iter().map(|u| u.value).sum::<u64>();

        prop_assert_eq!(
            build_unsigned_transaction(
                &mut utxos,
                vec![(BitcoinAddress::P2wpkhV0(dst_pkhash), total_value * 2)],
                BitcoinAddress::P2wpkhV0(main_pkhash),
                fee_per_vbyte
            ).expect_err("build transaction should fail because the amount is too high"),
            BuildTxError::NotEnoughFunds
        );
        prop_assert_eq!(&utxos_copy, &utxos);

        prop_assert_eq!(
            build_unsigned_transaction(
                &mut utxos,
                vec![(BitcoinAddress::P2wpkhV0(dst_pkhash), 1)],
                BitcoinAddress::P2wpkhV0(main_pkhash),
                fee_per_vbyte
            ).expect_err("build transaction should fail because the amount is too low to pay the fee"),
            BuildTxError::AmountTooLow
        );
        prop_assert_eq!(&utxos_copy, &utxos);
    }

    #[test]
    fn add_utxos_maintains_invariants(
        utxos_acc_idx in pvec((arb_utxo(5_000u64..1_000_000_000), 0..5usize), 10..20),
        accounts in pvec(arb_account(), 5),
    ) {
        let mut state = CkBtcMinterState::from(InitArgs {
            btc_network: Network::Regtest,
            ecdsa_key_name: "".to_string(),
            retrieve_btc_min_amount: 0,
            ledger_id: CanisterId::from_u64(42),
            max_time_in_queue_nanos: 0,
            min_confirmations: None,
        });
        for (utxo, acc_idx) in utxos_acc_idx {
            state.add_utxos(accounts[acc_idx].clone(), vec![utxo]);
            state.check_invariants().expect("invariant check failed");
        }
    }

    #[test]
    fn batching_preserves_invariants(
        utxos_acc_idx in pvec((arb_utxo(5_000u64..1_000_000_000), 0..5usize), 10..20),
        accounts in pvec(arb_account(), 5),
        requests in arb_retrieve_btc_requests(5_000u64..1_000_000_000, 1..25),
    ) {
        let mut state = CkBtcMinterState::from(InitArgs {
            btc_network: Network::Regtest,
            ecdsa_key_name: "".to_string(),
            retrieve_btc_min_amount: 5_000u64,
            ledger_id: CanisterId::from_u64(42),
            max_time_in_queue_nanos: 0,
            min_confirmations: None,
        });

        let mut available_amount = 0;
        for (utxo, acc_idx) in utxos_acc_idx {
            available_amount += utxo.value;
            state.add_utxos(accounts[acc_idx].clone(), vec![utxo]);
        }
        for req in requests {
            let block_index = req.block_index;
            state.push_back_pending_request(req);
            prop_assert_eq!(state.retrieve_btc_status(block_index), RetrieveBtcStatus::Pending);
        }

        let batch = state.build_batch();

        for req in batch.iter() {
            prop_assert_eq!(state.retrieve_btc_status(req.block_index), RetrieveBtcStatus::Unknown);
        }

        prop_assert!(batch.iter().map(|req| req.amount).sum::<u64>() <= available_amount);

        state.check_invariants().expect("invariant check failed");
    }

    #[test]
    fn btc_v0_p2wpkh_address_parsing(mut pkbytes in pvec(any::<u8>(), 32)) {
        use crate::address::network_and_public_key_to_p2wpkh;
        pkbytes.insert(0, 0x02);

        for network in [Network::Mainnet, Network::Testnet, Network::Regtest].iter() {
            let addr = network_and_public_key_to_p2wpkh(*network, &pkbytes);
            prop_assert_eq!(
                Ok(BitcoinAddress::P2wpkhV0(tx::hash160(&pkbytes))),
                BitcoinAddress::parse(&addr, *network)
            );
        }
    }

    #[test]
    fn btc_address_parsing_model(mut pkbytes in pvec(any::<u8>(), 32)) {
        pkbytes.insert(0, 0x02);

        let pk_result = bitcoin::PublicKey::from_slice(&pkbytes);

        prop_assume!(pk_result.is_ok());

        let pk = pk_result.unwrap();
        let pkhash = tx::hash160(&pkbytes);

        for network in [Network::Mainnet, Network::Testnet, Network::Regtest].iter() {
            let btc_net = network_to_btc_network(*network);
            let btc_addr = bitcoin::Address::p2pkh(&pk, btc_net);
            prop_assert_eq!(
                Ok(BitcoinAddress::P2pkh(tx::hash160(&pkbytes))),
                BitcoinAddress::parse(&btc_addr.to_string(), *network)
            );

            let btc_addr = bitcoin::Address::p2wpkh(&pk, btc_net).unwrap();
            prop_assert_eq!(
                Ok(BitcoinAddress::P2wpkhV0(pkhash)),
                BitcoinAddress::parse(&btc_addr.to_string(), *network)
            );
        }
    }

    #[test]
    fn btc_address_display_model(address in arb_address()) {
        for network in [Network::Mainnet, Network::Testnet].iter() {
            let addr_str = address.display(*network);
            let btc_addr = address_to_btc_address(&address, *network);
            prop_assert_eq!(btc_addr, bitcoin::Address::from_str(&addr_str).unwrap());
        }
    }

    #[test]
    fn address_roundtrip(address in arb_address()) {
        for network in [Network::Mainnet, Network::Testnet, Network::Regtest].iter() {
            let addr_str = address.display(*network);
            prop_assert_eq!(BitcoinAddress::parse(&addr_str, *network), Ok(address.clone()));
        }
    }

    #[test]
    fn sec1_to_der_positive_parses(sig in pvec(1u8..0x0f, 64)) {
        use simple_asn1::{from_der, ASN1Block::{Sequence, Integer}};

        let der = crate::signature::sec1_to_der(&sig);
        let decoded = from_der(&der).expect("failed to decode DER");
        if let[Sequence(_, items)] = &decoded[..] {
            if let [Integer(_, r), Integer(_, s)] = &items[..] {
                let (_, r_be) = r.to_bytes_be();
                let (_, s_be) = s.to_bytes_be();
                prop_assert_eq!(&r_be[..], &sig[..32]);
                prop_assert_eq!(&s_be[..], &sig[32..]);
                return Ok(());
            }
        }
        prop_assert!(false, "expected a DER sequence with two items, got: {:?}", decoded);
    }

    #[test]
    fn sec1_to_der_non_zero_parses(sig in pvec(any::<u8>(), 64)) {
        use simple_asn1::{from_der, ASN1Block::{Sequence, Integer}};

        prop_assume!(sig[..32].iter().any(|x| *x > 0));
        prop_assume!(sig[32..].iter().any(|x| *x > 0));

        let der = crate::signature::sec1_to_der(&sig);
        let decoded = from_der(&der).expect("failed to decode DER");

        if let[Sequence(_, items)] = &decoded[..] {
            if let [Integer(_, _r), Integer(_, _s)] = &items[..] {
                return Ok(());
            }
        }
        prop_assert!(false, "expected a DER sequence with two items, got: {:?}", decoded);
    }

    #[test]
    fn encode_valid_signatures(sig in pvec(any::<u8>(), 64)) {
        prop_assume!(sig[..32].iter().any(|x| *x > 0));
        prop_assume!(sig[32..].iter().any(|x| *x > 0));

        let encoded = crate::signature::EncodedSignature::from_sec1(&sig);
        crate::signature::validate_encoded_signature(encoded.as_slice()).expect("invalid signature");
    }

    #[test]
    fn amount_distribute_props(amount in any::<u64>(), n in 1..20u64) {
        let shares = crate::distribute(amount, n);

        // Distribute respects the share number.
        prop_assert_eq!(shares.len(), n as usize);

        // Distribute preserves the total.
        prop_assert_eq!(amount, shares.iter().sum::<u64>());

        // Distribute is fair
        for x in shares.iter() {
            for y in shares.iter() {
                prop_assert!(x.max(y) - x.min(y) <= 1);
            }
        }
    }

}
