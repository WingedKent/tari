// Copyright 2019. The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use crate::{
    base_node::{
        comms_interface::{CommsInterfaceError, OutboundNodeCommsInterface},
        service::{BaseNodeServiceConfig, BaseNodeServiceInitializer},
    },
    blocks::{genesis_block::get_genesis_block, BlockHeader},
    chain_storage::{BlockchainDatabase, DbTransaction, MemoryDatabase},
    consts::BASE_NODE_SERVICE_DESIRED_RESPONSE_FRACTION,
    test_utils::builders::{
        add_block_and_update_header,
        chain_block,
        create_genesis_block,
        create_test_kernel,
        create_utxo,
    },
};
use futures::Sink;
use rand::{distributions::Alphanumeric, rngs::OsRng, Rng};
use std::{error::Error, iter, sync::Arc, time::Duration};
use tari_comms::{
    builder::CommsNode,
    control_service::ControlServiceConfig,
    peer_manager::{NodeIdentity, Peer, PeerFeatures, PeerFlags},
};
use tari_comms_dht::Dht;
use tari_p2p::{
    comms_connector::{pubsub_connector, InboundDomainConnector, PeerMessage},
    initialization::{initialize_comms, CommsConfig},
    services::comms_outbound::CommsOutboundServiceInitializer,
};
use tari_service_framework::StackBuilder;
use tari_test_utils::address::get_next_local_address;
use tari_transactions::{tari_amount::MicroTari, types::HashDigest};
use tari_utilities::hash::Hashable;
use tempdir::TempDir;
use tokio::runtime::{Runtime, TaskExecutor};

fn random_string(len: usize) -> String {
    let mut rng = OsRng::new().unwrap();
    iter::repeat(()).map(|_| rng.sample(Alphanumeric)).take(len).collect()
}

fn setup_comms_services<TSink>(
    executor: TaskExecutor,
    node_identity: Arc<NodeIdentity>,
    peers: Vec<NodeIdentity>,
    publisher: InboundDomainConnector<TSink>,
) -> (CommsNode, Dht)
where
    TSink: Sink<Arc<PeerMessage>> + Clone + Unpin + Send + Sync + 'static,
    TSink::Error: Error + Send + Sync,
{
    let comms_config = CommsConfig {
        node_identity: Arc::clone(&node_identity),
        host: "127.0.0.1".parse().unwrap(),
        socks_proxy_address: None,
        control_service: ControlServiceConfig {
            listener_address: node_identity.control_service_address(),
            socks_proxy_address: None,
            requested_connection_timeout: Duration::from_millis(2000),
        },
        datastore_path: TempDir::new(random_string(8).as_str())
            .unwrap()
            .path()
            .to_str()
            .unwrap()
            .to_string(),
        establish_connection_timeout: Duration::from_secs(5),
        peer_database_name: random_string(8),
        inbound_buffer_size: 100,
        outbound_buffer_size: 100,
        dht: Default::default(),
    };

    let (comms, dht) = initialize_comms(executor, comms_config, publisher).unwrap();

    for p in peers {
        let addr = p.control_service_address();
        let NodeIdentity { identity, .. } = p;
        comms
            .peer_manager()
            .add_peer(Peer::new(
                identity.public_key,
                identity.node_id,
                addr.into(),
                PeerFlags::empty(),
                PeerFeatures::empty(),
            ))
            .unwrap();
    }

    (comms, dht)
}

pub fn setup_base_node_service(
    runtime: &Runtime,
    node_identity: NodeIdentity,
    peers: Vec<NodeIdentity>,
    blockchain_db: BlockchainDatabase<MemoryDatabase<HashDigest>>,
    config: BaseNodeServiceConfig,
) -> (OutboundNodeCommsInterface, CommsNode)
{
    let (publisher, subscription_factory) = pubsub_connector(runtime.executor(), 100);
    let subscription_factory = Arc::new(subscription_factory);
    let (comms, dht) = setup_comms_services(runtime.executor(), Arc::new(node_identity), peers, publisher);

    let fut = StackBuilder::new(runtime.executor(), comms.shutdown_signal())
        .add_initializer(CommsOutboundServiceInitializer::new(dht.outbound_requester()))
        .add_initializer(BaseNodeServiceInitializer::new(
            subscription_factory,
            blockchain_db,
            config,
        ))
        .finish();

    let handles = runtime.block_on(fut).expect("Service initialization failed");

    let outbound_nci = handles.get_handle::<OutboundNodeCommsInterface>().unwrap();

    (outbound_nci, comms)
}

fn create_network_with_3_base_nodes(
    runtime: &Runtime,
    config: BaseNodeServiceConfig,
) -> (
    OutboundNodeCommsInterface,
    OutboundNodeCommsInterface,
    OutboundNodeCommsInterface,
    BlockchainDatabase<MemoryDatabase<HashDigest>>,
    BlockchainDatabase<MemoryDatabase<HashDigest>>,
    BlockchainDatabase<MemoryDatabase<HashDigest>>,
    CommsNode,
    CommsNode,
    CommsNode,
)
{
    let mut rng = OsRng::new().unwrap();
    let alice_node_identity = NodeIdentity::random(
        &mut rng,
        get_next_local_address().parse().unwrap(),
        PeerFeatures::COMMUNICATION_NODE,
    )
    .unwrap();
    let alice_blockchain_db = BlockchainDatabase::new(MemoryDatabase::<HashDigest>::default()).unwrap();

    let bob_node_identity = NodeIdentity::random(
        &mut rng,
        get_next_local_address().parse().unwrap(),
        PeerFeatures::COMMUNICATION_NODE,
    )
    .unwrap();
    let bob_blockchain_db = BlockchainDatabase::new(MemoryDatabase::<HashDigest>::default()).unwrap();

    let carol_node_identity = NodeIdentity::random(
        &mut rng,
        get_next_local_address().parse().unwrap(),
        PeerFeatures::COMMUNICATION_NODE,
    )
    .unwrap();
    let carol_blockchain_db = BlockchainDatabase::new(MemoryDatabase::<HashDigest>::default()).unwrap();

    let (alice_outbound_nci, alice_comms) = setup_base_node_service(
        &runtime,
        alice_node_identity.clone(),
        vec![bob_node_identity.clone(), carol_node_identity.clone()],
        alice_blockchain_db.clone(),
        config.clone(),
    );
    let (bob_outbound_nci, bob_comms) = setup_base_node_service(
        &runtime,
        bob_node_identity.clone(),
        vec![alice_node_identity.clone(), carol_node_identity.clone()],
        bob_blockchain_db.clone(),
        config.clone(),
    );
    let (carol_outbound_nci, carol_comms) = setup_base_node_service(
        &runtime,
        carol_node_identity,
        vec![alice_node_identity, bob_node_identity],
        carol_blockchain_db.clone(),
        config,
    );
    (
        alice_outbound_nci,
        bob_outbound_nci,
        carol_outbound_nci,
        alice_blockchain_db,
        bob_blockchain_db,
        carol_blockchain_db,
        alice_comms,
        bob_comms,
        carol_comms,
    )
}

#[test]
fn request_response_get_metadata() {
    let runtime = Runtime::new().unwrap();

    let (mut alice_outbound_nci, _, _, _, bob_blockchain_db, _, alice_comms, bob_comms, carol_comms) =
        create_network_with_3_base_nodes(&runtime, BaseNodeServiceConfig::default());

    add_block_and_update_header(&bob_blockchain_db, create_genesis_block().0);

    runtime.block_on(async {
        let received_metadata = alice_outbound_nci.get_metadata().await.unwrap();
        assert_eq!(received_metadata.len(), 2);
        assert!(
            (received_metadata[0].height_of_longest_chain == None) ||
                (received_metadata[1].height_of_longest_chain == None)
        );
        assert!(
            (received_metadata[0].height_of_longest_chain == Some(0)) ||
                (received_metadata[1].height_of_longest_chain == Some(0))
        );
    });

    alice_comms.shutdown().unwrap();
    bob_comms.shutdown().unwrap();
    carol_comms.shutdown().unwrap();
}

#[test]
fn request_and_response_fetch_headers() {
    let runtime = Runtime::new().unwrap();

    let (mut alice_outbound_nci, _, _, _, bob_blockchain_db, carol_blockchain_db, alice_comms, bob_comms, carol_comms) =
        create_network_with_3_base_nodes(&runtime, BaseNodeServiceConfig::default());

    let mut headerb1 = BlockHeader::new(0);
    headerb1.height = 1;
    let mut headerb2 = BlockHeader::new(0);
    headerb2.height = 2;
    let mut txn = DbTransaction::new();
    txn.insert_header(headerb1.clone());
    txn.insert_header(headerb2.clone());
    assert!(bob_blockchain_db.commit(txn).is_ok());

    let mut headerc1 = BlockHeader::new(0);
    headerc1.height = 1;
    let mut headerc2 = BlockHeader::new(0);
    headerc2.height = 2;
    let mut txn = DbTransaction::new();
    txn.insert_header(headerc1.clone());
    txn.insert_header(headerc2.clone());
    assert!(carol_blockchain_db.commit(txn).is_ok());

    // The request is sent to a random remote base node so the returned headers can be from bob or carol
    runtime.block_on(async {
        let received_headers = alice_outbound_nci.fetch_headers(vec![1]).await.unwrap();
        assert_eq!(received_headers.len(), 1);
        assert!(received_headers.contains(&headerb1) || received_headers.contains(&headerc1));

        let received_headers = alice_outbound_nci.fetch_headers(vec![1, 2]).await.unwrap();
        assert_eq!(received_headers.len(), 2);
        assert!(
            (received_headers.contains(&headerb1) && (received_headers.contains(&headerb2))) ||
                (received_headers.contains(&headerc1) && (received_headers.contains(&headerc2)))
        );
    });

    alice_comms.shutdown().unwrap();
    bob_comms.shutdown().unwrap();
    carol_comms.shutdown().unwrap();
}

#[test]
fn request_and_response_fetch_kernels() {
    let runtime = Runtime::new().unwrap();

    let (mut alice_outbound_nci, _, _, _, bob_blockchain_db, carol_blockchain_db, alice_comms, bob_comms, carol_comms) =
        create_network_with_3_base_nodes(&runtime, BaseNodeServiceConfig::default());

    let kernel1 = create_test_kernel(5.into(), 0);
    let kernel2 = create_test_kernel(10.into(), 1);
    let hash1 = kernel1.hash();
    let hash2 = kernel2.hash();

    let mut txn = DbTransaction::new();
    txn.insert_kernel(kernel1.clone());
    txn.insert_kernel(kernel2.clone());
    assert!(bob_blockchain_db.commit(txn).is_ok());
    let mut txn = DbTransaction::new();
    txn.insert_kernel(kernel1.clone());
    txn.insert_kernel(kernel2.clone());
    assert!(carol_blockchain_db.commit(txn).is_ok());

    runtime.block_on(async {
        let received_kernels = alice_outbound_nci.fetch_kernels(vec![hash1.clone()]).await.unwrap();
        assert_eq!(received_kernels.len(), 1);
        assert_eq!(received_kernels[0], kernel1);

        let received_kernels = alice_outbound_nci.fetch_kernels(vec![hash1, hash2]).await.unwrap();
        assert_eq!(received_kernels.len(), 2);
        assert!(received_kernels.contains(&kernel1));
        assert!(received_kernels.contains(&kernel2));
    });

    alice_comms.shutdown().unwrap();
    bob_comms.shutdown().unwrap();
    carol_comms.shutdown().unwrap();
}

#[test]
fn request_and_response_fetch_utxos() {
    let runtime = Runtime::new().unwrap();

    let (mut alice_outbound_nci, _, _, _, bob_blockchain_db, carol_blockchain_db, alice_comms, bob_comms, carol_comms) =
        create_network_with_3_base_nodes(&runtime, BaseNodeServiceConfig::default());

    let (utxo1, _) = create_utxo(MicroTari(10_000));
    let (utxo2, _) = create_utxo(MicroTari(15_000));
    let hash1 = utxo1.hash();
    let hash2 = utxo2.hash();

    let mut txn = DbTransaction::new();
    txn.insert_utxo(utxo1.clone());
    txn.insert_utxo(utxo2.clone());
    assert!(bob_blockchain_db.commit(txn).is_ok());
    let mut txn = DbTransaction::new();
    txn.insert_utxo(utxo1.clone());
    txn.insert_utxo(utxo2.clone());
    assert!(carol_blockchain_db.commit(txn).is_ok());

    runtime.block_on(async {
        let received_utxos = alice_outbound_nci.fetch_utxos(vec![hash1.clone()]).await.unwrap();
        assert_eq!(received_utxos.len(), 1);
        assert_eq!(received_utxos[0], utxo1);

        let received_utxos = alice_outbound_nci.fetch_utxos(vec![hash1, hash2]).await.unwrap();
        assert_eq!(received_utxos.len(), 2);
        assert!(received_utxos.contains(&utxo1));
        assert!(received_utxos.contains(&utxo2));
    });

    alice_comms.shutdown().unwrap();
    bob_comms.shutdown().unwrap();
    carol_comms.shutdown().unwrap();
}

#[test]
fn request_and_response_fetch_blocks() {
    let runtime = Runtime::new().unwrap();

    let (mut alice_outbound_nci, _, _, _, bob_blockchain_db, carol_blockchain_db, alice_comms, bob_comms, carol_comms) =
        create_network_with_3_base_nodes(&runtime, BaseNodeServiceConfig::default());

    let block0 = add_block_and_update_header(&bob_blockchain_db, get_genesis_block());
    let mut block1 = chain_block(&block0, vec![]);
    block1 = add_block_and_update_header(&bob_blockchain_db, block1);
    let mut block2 = chain_block(&block1, vec![]);
    block2 = add_block_and_update_header(&bob_blockchain_db, block2);

    carol_blockchain_db.add_new_block(block0.clone()).unwrap();
    carol_blockchain_db.add_new_block(block1.clone()).unwrap();
    carol_blockchain_db.add_new_block(block2.clone()).unwrap();

    runtime.block_on(async {
        let received_blocks = alice_outbound_nci.fetch_blocks(vec![0]).await.unwrap();
        assert_eq!(received_blocks.len(), 1);
        assert_eq!(*received_blocks[0].block(), block0);

        let received_blocks = alice_outbound_nci.fetch_blocks(vec![0, 1]).await.unwrap();
        assert_eq!(received_blocks.len(), 2);
        assert_ne!(*received_blocks[0].block(), *received_blocks[1].block());
        assert!((*received_blocks[0].block() == block0) || (*received_blocks[1].block() == block0));
        assert!((*received_blocks[0].block() == block1) || (*received_blocks[1].block() == block1));
    });

    alice_comms.shutdown().unwrap();
    bob_comms.shutdown().unwrap();
    carol_comms.shutdown().unwrap();
}

#[test]
fn service_request_timeout() {
    let runtime = Runtime::new().unwrap();
    let mut rng = OsRng::new().unwrap();
    let base_node_service_config = BaseNodeServiceConfig {
        request_timeout: Duration::from_millis(10),
        desired_response_fraction: BASE_NODE_SERVICE_DESIRED_RESPONSE_FRACTION,
    };

    let alice_node_identity = NodeIdentity::random(
        &mut rng,
        get_next_local_address().parse().unwrap(),
        PeerFeatures::COMMUNICATION_NODE,
    )
    .unwrap();
    let alice_blockchain_db = BlockchainDatabase::new(MemoryDatabase::<HashDigest>::default()).unwrap();

    let bob_node_identity = NodeIdentity::random(
        &mut rng,
        get_next_local_address().parse().unwrap(),
        PeerFeatures::COMMUNICATION_NODE,
    )
    .unwrap();
    let bob_blockchain_db = BlockchainDatabase::new(MemoryDatabase::<HashDigest>::default()).unwrap();

    let (mut alice_outbound_nci, alice_comms) = setup_base_node_service(
        &runtime,
        alice_node_identity.clone(),
        vec![bob_node_identity.clone()],
        alice_blockchain_db,
        base_node_service_config.clone(),
    );
    let (_bob_outbound_nci, bob_comms) = setup_base_node_service(
        &runtime,
        bob_node_identity.clone(),
        vec![alice_node_identity.clone()],
        bob_blockchain_db.clone(),
        base_node_service_config,
    );

    runtime.block_on(async {
        assert_eq!(
            alice_outbound_nci.get_metadata().await,
            Err(CommsInterfaceError::RequestTimedOut)
        );
    });

    alice_comms.shutdown().unwrap();
    bob_comms.shutdown().unwrap();
}
