#[cfg(test)]
mod test {
    use wg_2024::controller::{DroneCommand, DroneEvent};
    use wg_2024::drone::Drone;
    //use wg_2024::controller;
    use wg_2024::network::NodeId;
    use wg_2024::packet::{Ack, Nack, NackType, NodeType, Packet, PacketType};
    //use wg_2024::config::{Config};
    use crossbeam_channel::unbounded;
    use wg_2024::network::SourceRoutingHeader;

    use crate::drone::RustDoIt;
    use std::collections::HashMap;
    use std::thread;
    use wg_2024::packet::Fragment;
    use wg_2024::packet::PacketType::{FloodRequest, FloodResponse};
    use log::info;
    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }
    fn create_sample_packet() -> Packet {
        Packet {
            pack_type: PacketType::MsgFragment(Fragment {
                fragment_index: 1,
                total_n_fragments: 1,
                length: 128,
                data: [1; 128],
            }),
            routing_header: SourceRoutingHeader {
                hop_index: 1,
                hops: vec![1, 11, 12, 21],
            },
            session_id: 1,
        }
    }

    fn create_custom_routing_header(hop_index: usize, hops: Vec<NodeId>) -> SourceRoutingHeader {
        SourceRoutingHeader { hop_index, hops }
    }

    fn create_custom_packet(
        source_routing_header: SourceRoutingHeader,
        packet_type: PacketType,
        session_id: u64,
    ) -> Packet {
        Packet {
            pack_type: packet_type,
            routing_header: source_routing_header,
            session_id,
        }
    }

    #[test]
    //#[cfg(feature = "partial_eq")]
    /// Test forward functionality of a generic packet for a drone
    pub fn generic_packet_forward() {
        init();
        let (d_send, d_recv) = unbounded();
        let (d2_send, d2_recv) = unbounded::<Packet>();
        let (d_command_send, d_command_recv) = unbounded();
        let (d_event_send, d_event_recv) = unbounded();
        let neighbours = HashMap::from([(12, d2_send.clone())]);
        let mut drone11 = RustDoIt::new(
            11,
            d_event_send,
            d_command_recv,
            d_recv.clone(),
            neighbours,
            0.0,
        );
        thread::spawn(move || {
            drone11.run();
        });

        let mut msg = create_sample_packet();

        // "Client" sends packet to d
        d_send.send(msg.clone()).unwrap();
        msg.routing_header.hop_index += 1;
        let packet_sent_event = DroneEvent::PacketSent(msg.clone());

        // d2 receives packet from d1
        let packet_received = d2_recv.recv().unwrap();
        let event_log = d_event_recv.recv().unwrap();

        assert_eq!(packet_received, msg);
        assert_eq!(event_log, packet_sent_event);

        d_command_send.send(DroneCommand::Crash).unwrap()
    }

    #[test]
    //#[cfg(feature = "partial_eq")]
    /// Test forward functionality of a nack for a drone
    pub fn generic_nack_forward() {
        init();
        let (d1_send, d1_recv) = unbounded();
        let (d2_send, d2_recv) = unbounded::<Packet>();
        let (d_command_send, d_command_recv) = unbounded();
        let (d_event_send, d_event_recv) = unbounded();
        let neighbours = HashMap::from([(1, d2_send.clone())]);

        let mut drone = RustDoIt::new(
            11,
            d_event_send,
            d_command_recv,
            d1_recv.clone(),
            neighbours,
            0.0,
        );
        thread::spawn(move || {
            drone.run();
        });

        let mut nack = Packet {
            pack_type: PacketType::Nack(Nack {
                fragment_index: 1,
                nack_type: NackType::DestinationIsDrone,
            }),
            routing_header: SourceRoutingHeader {
                hop_index: 1,
                hops: vec![12, 11, 1],
            },
            session_id: 1,
        };

        // Hop12 sends packet to drone
        d1_send.send(nack.clone()).unwrap();
        let event = d_event_recv.recv().unwrap();

        nack.routing_header.hop_index += 1;
        let packet_sent_event = DroneEvent::PacketSent(nack.clone());
        let packet_received = d2_recv.recv().unwrap();

        assert_eq!(packet_received, nack);
        assert_eq!(event, packet_sent_event);
        d_command_send.send(DroneCommand::Crash).unwrap()
    }

    #[test]
    //#[cfg(feature = "partial_eq")]
    /// Checks if the packet is dropped by a drone and a Nack is sent back. The drone MUST have 100% packet drop rate, otherwise the test will fail sometimes.
    pub fn generic_fragment_drop() {
        init();

        // Client 1
        let (c_send, c_recv) = unbounded();
        // Drone 11
        let (d_send, d_recv) = unbounded();
        // SC commands
        let (d_command_send, d_command_recv) = unbounded();
        let (d_event_send, d_event_recv) = unbounded();

        let neighbours = HashMap::from([(12, d_send.clone()), (1, c_send.clone())]);
        let mut drone = RustDoIt::new(
            11,
            d_event_send,
            d_command_recv,
            d_recv.clone(),
            neighbours,
            1.0,
        );

        // Spawn the drone's run method in a separate thread
        thread::spawn(move || {
            drone.run();
        });

        let msg = create_sample_packet();

        // "Client" sends packet to the drone
        d_send.send(msg.clone()).unwrap();

        let dropped = Nack {
            fragment_index: msg.get_fragment_index(),
            nack_type: NackType::Dropped,
        };
        let srh = SourceRoutingHeader {
            hop_index: 1,
            hops: vec![11, 1],
        };
        let nack_packet = Packet {
            pack_type: PacketType::Nack(dropped),
            routing_header: srh,
            session_id: 1,
        };

        // msg.routing_header.hop_index += 1;

        let packet_sent_event = DroneEvent::PacketSent(nack_packet.clone());
        let packet_drop_event = DroneEvent::PacketDropped(msg.clone());
        let packet_dropped = d_event_recv.recv().unwrap();
        let nack_sent = d_event_recv.recv().unwrap();

        assert_eq!(packet_sent_event, nack_sent);
        assert_eq!(packet_drop_event, packet_dropped);
        assert_eq!(c_recv.recv().unwrap(), nack_packet);
        d_command_send.send(DroneCommand::Crash).unwrap()
    }

    #[test]
    fn ack_forward() {
        init();

        let (d1_send, d1_recv) = unbounded();
        let (d2_send, d2_recv) = unbounded::<Packet>();
        let (c_send, _c_recv) = unbounded();
        let (d_command_send, d_command_recv) = unbounded();
        let (d_event_send, _d_event_recv) = unbounded();

        let neighbours11 = HashMap::from([(1, c_send.clone()), (12, d2_send.clone())]);
        let mut drone = RustDoIt::new(
            11,
            d_event_send.clone(),
            d_command_recv.clone(),
            d1_recv.clone(),
            neighbours11,
            0.0,
        );

        thread::spawn(move || {
            drone.run();
        });

        let mut ack = Packet {
            pack_type: PacketType::Ack(Ack { fragment_index: 1 }),
            routing_header: SourceRoutingHeader {
                hop_index: 1,
                hops: vec![1, 11, 12],
            },
            session_id: 1,
        };

        d1_send.send(ack.clone()).unwrap();

        let got = d2_recv.recv().unwrap();
        ack.routing_header.hop_index += 1;

        assert_eq!(ack, got);
        d_command_send.send(DroneCommand::Crash).unwrap();

        ack.routing_header.hop_index -= 1;
        d1_send.send(ack.clone()).unwrap();
        let got = d2_recv.recv().unwrap();
        ack.routing_header.hop_index += 1;
        assert_eq!(ack, got);
    }

    pub fn ack_to_crashed_sender() {
        init();

        let (d1_send, d1_recv) = unbounded();
        let (c_send, _c_recv) = unbounded();
        let (d_command_send, d_command_recv) = unbounded();
        let (d_event_send, d_event_recv) = unbounded();

        let neighbours11 = HashMap::from([(1, c_send.clone())]);
        let mut drone = RustDoIt::new(
            11,
            d_event_send.clone(),
            d_command_recv.clone(),
            d1_recv.clone(),
            neighbours11,
            0.0,
        );

        thread::spawn(move || {
            drone.run();
        });

        let mut ack = Packet {
            pack_type: PacketType::Ack(Ack { fragment_index: 1 }),
            routing_header: SourceRoutingHeader {
                hop_index: 1,
                hops: vec![1, 11, 12],
            },
            session_id: 1,
        };

        d1_send.send(ack.clone()).unwrap();

        let got = match d_event_recv.recv().unwrap() {
            DroneEvent::ControllerShortcut(p )=> p,
            _ => panic!("Expected ControllerShortcut"),
        };
        ack.routing_header.hop_index += 1;

        assert_eq!(ack, got);

    }

    #[test]
    /// Checks if the packet is dropped by the second drone and a Nack is sent back. The first drone must have 0% PDR and the second one 100% PDR, otherwise the test will fail sometimes.
    pub fn generic_chain_fragment_drop() {
        init();

        // Client 1 channels
        let (c_send, c_recv) = unbounded();
        // Server 21 channels
        let (s_send, _s_recv) = unbounded();
        // Drone 11
        let (d_send, d_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (d_command_send, d_command_recv) = unbounded();
        let (d_event_send, d_event_recv) = unbounded();

        // Drone 11
        let neighbours11 = HashMap::from([(12, d12_send.clone()), (1, c_send.clone())]);
        let mut drone1 = RustDoIt::new(
            11,
            d_event_send.clone(),
            d_command_recv.clone(),
            d_recv.clone(),
            neighbours11,
            0.0,
        );
        // Drone 12
        let neighbours12 = HashMap::from([(11, d_send.clone()), (21, s_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            d_event_send.clone(),
            d_command_recv.clone(),
            d12_recv.clone(),
            neighbours12,
            1.0,
        );

        // Spawn the drone's run method in a separate thread
        thread::spawn(move || {
            drone1.run();
        });
        thread::spawn(move || {
            drone2.run();
        });

        let mut msg = create_sample_packet();

        // "Client" sends packet to the drone1
        d_send.send(msg.clone()).unwrap();

        // Client receive an NACK originated from drone2
        let mut packet_true = Packet {
            pack_type: PacketType::Nack(Nack {
                fragment_index: msg.get_fragment_index(),
                nack_type: NackType::Dropped,
            }),
            routing_header: SourceRoutingHeader {
                hop_index: 2,
                hops: vec![12, 11, 1],
            },
            session_id: 1,
        };

        let packet_got = c_recv.recv().unwrap();
        assert_eq!(packet_true, packet_got);

        msg.routing_header.increase_hop_index();
        let packet_sent_event = DroneEvent::PacketSent(msg.clone());

        // msg.routing_header.increase_hop_index();
        let packet_drop_event = DroneEvent::PacketDropped(msg.clone());
        let packet_sent = d_event_recv.recv().unwrap();
        let packet_dropped = d_event_recv.recv().unwrap();
        packet_true.routing_header.decrease_hop_index();
        let nack_sent = d_event_recv.recv().unwrap();

        assert_eq!(packet_sent_event, packet_sent);
        assert_eq!(packet_drop_event, packet_dropped);
        assert_eq!(nack_sent, DroneEvent::PacketSent(packet_true.clone()));
        packet_true.routing_header.increase_hop_index();
        let nack_sent = d_event_recv.recv().unwrap();
        assert_eq!(nack_sent, DroneEvent::PacketSent(packet_true.clone()));

        d_command_send.send(DroneCommand::Crash).unwrap();
        d_command_send.send(DroneCommand::Crash).unwrap();
    }

    #[test]
    /// Test forward functionality of a generic packet for a chain of drones
    pub fn generic_chain_fragment_forward() {
        init();

        // Client 1 channels
        let (c_send, _c_recv) = unbounded();
        // Server 21 channels
        let (s_send, s_recv) = unbounded();
        // Drone 11
        let (d_send, d_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (d_command_send, d_command_recv) = unbounded();

        // Drone 11
        let neighbours11 = HashMap::from([(12, d12_send.clone()), (1, c_send.clone())]);
        let mut drone1 = RustDoIt::new(
            11,
            unbounded().0,
            d_command_recv.clone(),
            d_recv.clone(),
            neighbours11,
            0.0,
        );
        // Drone 12
        let neighbours12 = HashMap::from([(11, d_send.clone()), (21, s_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            unbounded().0,
            d_command_recv.clone(),
            d12_recv.clone(),
            neighbours12,
            0.0,
        );

        // Spawn the drone's run method in a separate thread
        thread::spawn(move || {
            drone1.run();
        });
        thread::spawn(move || {
            drone2.run();
        });

        let msg = create_sample_packet();

        // Client sends packet to the drone
        d_send.send(msg.clone()).unwrap();

        // Server receives a packet with the same content of msg but with hop_index+2
        let mut packet_true = msg.clone();
        packet_true.routing_header.hop_index += 2;

        let packet_got = s_recv.recv().unwrap();
        assert_eq!(packet_true, packet_got);
        d_command_send.send(DroneCommand::Crash).unwrap();
        d_command_send.send(DroneCommand::Crash).unwrap()
    }

    #[test]
    /// Test the forward of a flood request coming from drone1, forwarded to drone2 and drone3
    pub fn flood_request_forward() {
        init();

        // Client 1 channels
        let (c_send, _c_recv) = unbounded();
        // Server 21 channels
        let (s_send, s_recv) = unbounded();
        // Drone 11
        let (d_send11, d11_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // Drone 13
        let (d13_send, d13_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (_d_command_send, d_command_recv) = unbounded();

        let (d_event_send, d_event_recv) = unbounded();

        // Drone 11
        let neighbours11 = HashMap::from([
            (12, d12_send.clone()),
            (13, d13_send.clone()),
            (1, c_send.clone()),
        ]);
        let mut drone1 = RustDoIt::new(
            11,
            d_event_send.clone(),
            d_command_recv.clone(),
            d11_recv.clone(),
            neighbours11,
            0.0,
        );
        // Drone 12
        let neighbours12 = HashMap::from([(11, d_send11.clone()), (21, s_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            d_event_send.clone(),
            d_command_recv.clone(),
            d12_recv.clone(),
            neighbours12,
            0.0,
        );
        let neighbours13 = HashMap::from([(11, d_send11.clone()), (21, s_send.clone())]);
        let mut drone3 = RustDoIt::new(
            13,
            d_event_send.clone(),
            d_command_recv.clone(),
            d13_recv.clone(),
            neighbours13,
            0.0,
        );

        // Spawn the drone's run method in a separate thread
        thread::spawn(move || {
            drone1.run();
        });
        thread::spawn(move || {
            drone2.run();
        });
        thread::spawn(move || {
            drone3.run();
        });

        let srh = create_custom_routing_header(0, vec![]);

        let flood_request = create_custom_packet(
            srh,
            FloodRequest(wg_2024::packet::FloodRequest {
                flood_id: 0,
                initiator_id: 1,
                path_trace: vec![(1, NodeType::Client)],
            }),
            0,
        );

        // Client sends packet to the drone1
        d_send11.send(flood_request.clone()).unwrap();

        let mut packet_true_1 = flood_request.clone();
        packet_true_1.pack_type = FloodRequest(wg_2024::packet::FloodRequest {
            flood_id: 0,
            initiator_id: 1,
            path_trace: vec![(1, NodeType::Client), (11, NodeType::Drone)],
        });

        // Server receives a packet with the same content of msg but with hop_index+2
        let mut packet_true_2 = flood_request.clone();
        packet_true_2.pack_type = FloodRequest(wg_2024::packet::FloodRequest {
            flood_id: 0,
            initiator_id: 1,
            path_trace: vec![
                (1, NodeType::Client),
                (11, NodeType::Drone),
                (12, NodeType::Drone),
            ],
        });

        let mut packet_true_3 = flood_request.clone();
        packet_true_3.pack_type = FloodRequest(wg_2024::packet::FloodRequest {
            flood_id: 0,
            initiator_id: 1,
            path_trace: vec![
                (1, NodeType::Client),
                (11, NodeType::Drone),
                (13, NodeType::Drone),
            ],
        });
        let packet_got = s_recv.recv().unwrap();
        assert!(packet_got == packet_true_2 || packet_got == packet_true_3);
        let packet_got = s_recv.recv().unwrap();
        assert!(packet_got == packet_true_2 || packet_got == packet_true_3);

        let packet_event = d_event_recv.recv().unwrap();
        assert_eq!(packet_event, DroneEvent::PacketSent(packet_true_1.clone()));
        let packet_event = d_event_recv.recv().unwrap();
        assert_eq!(packet_event, DroneEvent::PacketSent(packet_true_1.clone()));
        let packet_event = d_event_recv.recv().unwrap();
        assert!(
            packet_event == DroneEvent::PacketSent(packet_true_2.clone())
                || packet_event == DroneEvent::PacketSent(packet_true_3.clone())
        );
        let packet_event = d_event_recv.recv().unwrap();
        assert!(
            packet_event == DroneEvent::PacketSent(packet_true_2.clone())
                || packet_event == DroneEvent::PacketSent(packet_true_3.clone())
        );
    }

    #[test]
    /// Test the forward of a flood response coming from drone2, forwarded to drone1, forwarded again
    pub fn flood_response_forward() {
        init();

        // Client 1 channels
        let (c_send, c_recv) = unbounded();
        // Server 21 channels
        let (s_send, _s_recv) = unbounded();
        // Drone 11
        let (d_send, d_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (d_command_send, d_command_recv) = unbounded();

        // Drone 11
        let neighbours11 = HashMap::from([(12, d12_send.clone()), (1, c_send.clone())]);
        let mut drone1 = RustDoIt::new(
            11,
            unbounded().0,
            d_command_recv.clone(),
            d_recv.clone(),
            neighbours11,
            0.0,
        );
        // Drone 12
        let neighbours12 = HashMap::from([(11, d_send.clone()), (21, s_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            unbounded().0,
            d_command_recv.clone(),
            d12_recv.clone(),
            neighbours12,
            0.0,
        );

        // Spawn the drone's run method in a separate thread
        thread::spawn(move || {
            drone1.run();
        });
        thread::spawn(move || {
            drone2.run();
        });

        let srh = create_custom_routing_header(
            // to use in flood_response
            1,
            vec![21, 12, 11, 1],
        );

        let flood_response = create_custom_packet(
            srh,
            FloodResponse(wg_2024::packet::FloodResponse {
                flood_id: 0,
                path_trace: vec![
                    (1, NodeType::Client),
                    (11, NodeType::Drone),
                    (12, NodeType::Drone),
                    (21, NodeType::Server),
                ],
            }),
            0,
        );

        // Client sends packet to the drone
        d12_send.send(flood_response.clone()).unwrap();

        // Server receives a packet with the same content of msg but with hop_index+2
        let mut packet_true = flood_response.clone();
        packet_true.routing_header.hop_index += 2;
        let packet_got = c_recv.recv().unwrap();

        assert_eq!(packet_got, packet_true);
        d_command_send.send(DroneCommand::Crash).unwrap();
        d_command_send.send(DroneCommand::Crash).unwrap();
    }

    #[test]
    /// Test the generation of a flood response due to an isolated drone (only neighbour the one who sent the flood request)
    pub fn flood_response_isolation() {
        init();

        // Client 1 channels
        let (c_send, c_recv) = unbounded();
        // Drone 11
        let (d_send, d_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (d_command_send, d_command_recv) = unbounded();

        // Drone 11
        let neighbours11 = HashMap::from([(12, d12_send.clone()), (1, c_send.clone())]);
        let mut drone1 = RustDoIt::new(
            11,
            unbounded().0,
            d_command_recv.clone(),
            d_recv.clone(),
            neighbours11,
            0.0,
        );
        // Drone 12
        let neighbours12 = HashMap::from([(11, d_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            unbounded().0,
            d_command_recv.clone(),
            d12_recv.clone(),
            neighbours12,
            0.0,
        );

        // Spawn the drone's run method in a separate thread
        thread::spawn(move || {
            drone1.run();
        });
        thread::spawn(move || {
            drone2.run();
        });

        let srh = create_custom_routing_header(
            // to use in flood_response
            0,
            vec![],
        );

        let flood_request = create_custom_packet(
            srh,
            FloodRequest(wg_2024::packet::FloodRequest {
                flood_id: 0,
                initiator_id: 1,
                path_trace: vec![(1, NodeType::Client)],
            }),
            0,
        );

        // Client sends packet to the drone
        d_send.send(flood_request.clone()).unwrap();

        let srh = create_custom_routing_header(2, vec![12, 11, 1]);
        let packet_true = create_custom_packet(
            srh,
            FloodResponse(wg_2024::packet::FloodResponse {
                flood_id: 0,
                path_trace: vec![
                    (1, NodeType::Client),
                    (11, NodeType::Drone),
                    (12, NodeType::Drone),
                ],
            }),
            0,
        );

        let packet_got = c_recv.recv().unwrap();
        assert_eq!(packet_got, packet_true);
        d_command_send.send(DroneCommand::Crash).unwrap();
        d_command_send.send(DroneCommand::Crash).unwrap();
    }

    #[test]
    /// Test the generation of a flood response due to an already visited hop
    pub fn flood_response_visited() {
        init();

        let (c_send, _c_recv) = unbounded();
        // Server 21 channels
        let (s_send, s_recv) = unbounded();
        // Drone 11
        let (d11_send, d11_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // Drone 13
        let (d13_send, d13_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (d_command_send, d_command_recv) = unbounded();

        let (d_event_send, d_event_recv) = unbounded();

        // Drone 11
        let neighbours11 = HashMap::from([
            (12, d12_send.clone()),
            (13, d13_send.clone()),
            (1, c_send.clone()),
        ]);
        let mut drone1 = RustDoIt::new(
            11,
            d_event_send.clone(),
            d_command_recv.clone(),
            d11_recv.clone(),
            neighbours11,
            0.0,
        );
        // Drone 12
        let neighbours12 = HashMap::from([(11, d11_send.clone()), (13, d13_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            d_event_send.clone(),
            d_command_recv.clone(),
            d12_recv.clone(),
            neighbours12,
            0.0,
        );
        let neighbours13 = HashMap::from([
            (11, d11_send.clone()),
            (12, d12_send.clone()),
            (21, s_send.clone()),
        ]);
        let mut drone3 = RustDoIt::new(
            13,
            d_event_send.clone(),
            d_command_recv.clone(),
            d13_recv.clone(),
            neighbours13,
            0.0,
        );

        // Spawn the drone's run method in a separate thread
        thread::spawn(move || {
            drone1.run();
        });
        thread::spawn(move || {
            drone2.run();
        });
        thread::spawn(move || {
            drone3.run();
        });

        let srh = create_custom_routing_header(0, vec![]);

        let flood_request = create_custom_packet(
            srh,
            FloodRequest(wg_2024::packet::FloodRequest {
                flood_id: 0,
                initiator_id: 1,
                path_trace: vec![(1, NodeType::Client)],
            }),
            0,
        );

        // Client sends packet to the drone1
        d11_send.send(flood_request.clone()).unwrap();

        // Server receives a packet with the same content of msg but with hop_index+2
        let mut request_11_13 = flood_request.clone();
        request_11_13.pack_type = FloodRequest(wg_2024::packet::FloodRequest {
            flood_id: 0,
            initiator_id: 1,
            path_trace: vec![
                (1, NodeType::Client),
                (11, NodeType::Drone),
                (13, NodeType::Drone),
            ],
        });

        let mut request_11_12_13 = flood_request.clone();
        request_11_12_13.pack_type = FloodRequest(wg_2024::packet::FloodRequest {
            flood_id: 0,
            initiator_id: 1,
            path_trace: vec![
                (1, NodeType::Client),
                (11, NodeType::Drone),
                (12, NodeType::Drone),
                (13, NodeType::Drone),
            ],
        });

        let request_got = s_recv.recv().unwrap();
        assert!(request_got == request_11_12_13 || request_got == request_11_13);

        while let Ok(event) = d_event_recv.try_recv() {
            println!("{:?}", event);
        }

        d_command_send.send(DroneCommand::Crash).unwrap();
        d_command_send.send(DroneCommand::Crash).unwrap();
        d_command_send.send(DroneCommand::Crash).unwrap();
    }

    #[test]
    fn destination_is_drone() {
        init();

        let (c_send, c_recv) = unbounded();
        // Server 21 channels
        let (s_send, _s_recv) = unbounded();
        // Drone 11
        let (d11_send, d11_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (d_command_send, d_command_recv) = unbounded();

        let (d_event_send, _d_event_recv) = unbounded();

        let neighbours11 = HashMap::from([(12, d12_send.clone()), (1, c_send.clone())]);
        let mut drone1 = RustDoIt::new(
            11,
            d_event_send.clone(),
            d_command_recv.clone(),
            d11_recv.clone(),
            neighbours11,
            0.0,
        );

        let neighbours12 = HashMap::from([(11, d11_send.clone()), (21, s_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            d_event_send.clone(),
            d_command_recv.clone(),
            d12_recv.clone(),
            neighbours12,
            0.0,
        );

        thread::spawn(move || {
            drone1.run();
        });
        thread::spawn(move || {
            drone2.run();
        });

        let srh = SourceRoutingHeader::new(vec![1, 11, 12], 1);
        let packet = Packet {
            pack_type: PacketType::MsgFragment(Fragment {
                fragment_index: 1,
                total_n_fragments: 1,
                length: 128,
                data: [1; 128],
            }),
            routing_header: srh,
            session_id: 1,
        };

        d11_send.send(packet.clone()).unwrap();

        let expected = Packet {
            pack_type: PacketType::Nack(Nack {
                fragment_index: packet.get_fragment_index(),
                nack_type: NackType::DestinationIsDrone,
            }),
            routing_header: SourceRoutingHeader::new(vec![12, 11, 1], 2),
            session_id: 1,
        };

        let got = c_recv.recv().unwrap();
        assert_eq!(got, expected);
        d_command_send.send(DroneCommand::Crash).unwrap();
        d_command_send.send(DroneCommand::Crash).unwrap();
    }

    #[test]
    fn error_in_routing() {
        init();

        let (c_send, c_recv) = unbounded();
        // Server 21 channels
        let (s_send, _s_recv) = unbounded();
        // Drone 11
        let (d11_send, d11_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (d_command_send, d_command_recv) = unbounded();

        let (d_event_send, _d_event_recv) = unbounded();

        let neighbours11 = HashMap::from([(12, d12_send.clone()), (1, c_send.clone())]);
        let mut drone1 = RustDoIt::new(
            11,
            d_event_send.clone(),
            d_command_recv.clone(),
            d11_recv.clone(),
            neighbours11,
            0.0,
        );

        let neighbours12 = HashMap::from([(11, d11_send.clone()), (21, s_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            d_event_send.clone(),
            d_command_recv.clone(),
            d12_recv.clone(),
            neighbours12,
            0.0,
        );

        thread::spawn(move || {
            drone1.run();
        });
        thread::spawn(move || {
            drone2.run();
        });

        let srh = SourceRoutingHeader::new(vec![1, 11, 13], 1);
        let packet = Packet {
            pack_type: PacketType::MsgFragment(Fragment {
                fragment_index: 1,
                total_n_fragments: 1,
                length: 128,
                data: [1; 128],
            }),
            routing_header: srh,
            session_id: 1,
        };

        d11_send.send(packet.clone()).unwrap();

        let expected = Packet {
            pack_type: PacketType::Nack(Nack {
                fragment_index: packet.get_fragment_index(),
                nack_type: NackType::ErrorInRouting(13),
            }),
            routing_header: SourceRoutingHeader::new(vec![11, 1], 1),
            session_id: 1,
        };

        let got = c_recv.recv().unwrap();
        assert_eq!(got, expected);
        d_command_send.send(DroneCommand::Crash).unwrap();
        d_command_send.send(DroneCommand::Crash).unwrap();
    }

    #[test]
    fn unexpected_recipient() {
        init();

        let (c_send, c_recv) = unbounded();
        // Server 21 channels
        let (s_send, _s_recv) = unbounded();
        // Drone 11
        let (d11_send, d11_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (d_command_send, d_command_recv) = unbounded();

        let (d_event_send, _d_event_recv) = unbounded();

        let neighbours11 = HashMap::from([(12, d12_send.clone()), (1, c_send.clone())]);
        let mut drone1 = RustDoIt::new(
            11,
            d_event_send.clone(),
            d_command_recv.clone(),
            d11_recv.clone(),
            neighbours11,
            0.0,
        );

        let neighbours12 = HashMap::from([(11, d11_send.clone()), (21, s_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            d_event_send.clone(),
            d_command_recv.clone(),
            d12_recv.clone(),
            neighbours12,
            0.0,
        );

        thread::spawn(move || {
            drone1.run();
        });
        thread::spawn(move || {
            drone2.run();
        });

        let srh = SourceRoutingHeader::new(vec![1, 12, 11], 1);
        let packet = Packet {
            pack_type: PacketType::MsgFragment(Fragment {
                fragment_index: 1,
                total_n_fragments: 1,
                length: 128,
                data: [1; 128],
            }),
            routing_header: srh,
            session_id: 1,
        };

        d11_send.send(packet.clone()).unwrap();

        let expected = Packet {
            pack_type: PacketType::Nack(Nack {
                fragment_index: packet.get_fragment_index(),
                nack_type: NackType::UnexpectedRecipient(11),
            }),
            routing_header: SourceRoutingHeader::new(vec![11, 1], 1),
            session_id: 1,
        };

        let got = c_recv.recv().unwrap();
        assert_eq!(got, expected);
        d_command_send.send(DroneCommand::Crash).unwrap();
        d_command_send.send(DroneCommand::Crash).unwrap();
    }

    #[test]
    fn add_sender() {
        init();

        let (s_send, s_recv) = unbounded();

        let (d_send, d_recv) = unbounded();
        let (d_command_send, d_command_recv) = unbounded();
        let (d_event_send, _d_event_recv) = unbounded();
        let neighbours = HashMap::new();

        let mut drone = RustDoIt::new(
            11,
            d_event_send.clone(),
            d_command_recv.clone(),
            d_recv,
            neighbours,
            0.0,
        );

        thread::spawn(move || {
            drone.run();
        });

        let (d2_send, d2_recv) = unbounded();
        let (_d2_command_send, d2_command_recv) = unbounded();
        let neighbours = HashMap::from([(11, d_send.clone()), (21, s_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            d_event_send.clone(),
            d2_command_recv.clone(),
            d2_recv,
            neighbours,
            0.0,
        );

        thread::spawn(move || {
            drone2.run();
        });

        d_command_send
            .send(DroneCommand::AddSender(12, d2_send))
            .unwrap();
        let msg = Packet {
            pack_type: PacketType::MsgFragment(Fragment {
                fragment_index: 1,
                total_n_fragments: 1,
                length: 128,
                data: [1; 128],
            }),
            routing_header: SourceRoutingHeader::new(vec![1, 11, 12, 21], 1),
            session_id: 1,
        };

        // Client sends packet to the drone
        d_send.send(msg.clone()).unwrap();

        // Server receives a packet with the same content of msg but with hop_index+2
        let mut packet_true = msg.clone();
        packet_true.routing_header.hop_index += 2;

        let packet_got = s_recv.recv().unwrap();
        assert_eq!(packet_true, packet_got);
        d_command_send.send(DroneCommand::Crash).unwrap();
        d_command_send.send(DroneCommand::Crash).unwrap()
    }
}
