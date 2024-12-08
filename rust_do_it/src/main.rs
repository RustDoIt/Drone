#![allow(warnings)]

extern crate wg_2024;


use rand::Rng;
use wg_2024::drone::{Drone};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::controller;
use wg_2024::network::NodeId;
use wg_2024::packet::{Ack, Nack, NackType, Packet, PacketType, NodeType, FloodRequest, FloodResponse};
use wg_2024::config::{Config};
use wg_2024::network::{SourceRoutingHeader};
use crossbeam_channel::{select_biased, unbounded};
use crossbeam_channel::{Receiver, Sender};
use wg_2024::packet::Fragment;

use std::collections::{HashMap, HashSet};
use std::{fs, thread};
use std::ops::Index;

#[derive(Debug)]
struct RustDoIt {
    id: NodeId,
    controller_send: Sender<DroneEvent>,                // Used to send events to the controller (receiver is in the controller)
    controller_recv: Receiver<DroneCommand>,            // Used to receive commands from the controller (sender is in the controller)
    packet_recv: Receiver<Packet>,                      // The receiving end of the channel for receiving packets from drones
    packet_send: HashMap<NodeId, Sender<Packet>>,       // Mapping of drone IDs to senders, allowing packets to be sent to specific drones
    flood_session: HashSet<(u64,NodeId)>,
    pdr: f32,
}

impl Drone for RustDoIt {

    fn new(
        id: NodeId,
        controller_send: Sender<DroneEvent>,
        controller_recv: Receiver<DroneCommand>,
        packet_recv: Receiver<Packet>,
        packet_send: HashMap<NodeId, Sender<Packet>>,
        pdr: f32,
    ) -> Self {
        Self {
            id,
            controller_send,
            controller_recv,
            packet_recv,
            packet_send,
            flood_session: HashSet::new(),
            pdr,
        }
    }

    fn run(&mut self) {
        println!("Drone {} is Running", self.id);

        loop {
            // Use select_biased to handle incoming commands and packets in normal operation
            select_biased! {
                recv(self.controller_recv) -> command => {
                    if let Ok(command) = command {
                        if self.handle_command(command) {
                            // Crashing routine
                            while let Ok(packet) = self.packet_recv.try_recv() {
                                self.handle_packet_crash(packet);
                            }
                            println!("Drone {} has crashed, exiting loop", self.id);
                            self.drop_senders();
                            return;
                        }
                    } else {
                        println!("Error receiving from controller_recv");
                    }
                },
                recv(self.packet_recv) -> packet => {
                    if let Ok(packet) = packet {
                        self.handle_packet(packet);
                    } else {
                        println!("Error receiving from packet_recv");
                    }
                }
            }
        }
    }
}



impl RustDoIt {

    fn build_nack(
        mut routing_header: SourceRoutingHeader,
        fragment_index: u64,
        session_id: u64,
        nack_type: NackType
    ) -> Packet {

        routing_header.hops.reverse();                                                       // reverse the trace
        routing_header.hop_index = routing_header.hops.len() - routing_header.hop_index - 1; // starting node
        routing_header.hops = routing_header.hops.split_off(routing_header.hop_index);       // truncate and keep only useful path
        routing_header.hop_index = 1;                                                        // set hop_index to 1

        Packet {
            pack_type: PacketType::Nack(
                Nack {
                    fragment_index,
                    nack_type,
                }
            ),
            routing_header: routing_header.clone(),
            session_id,
        }
    }
    fn build_ack(
        mut routing_header: SourceRoutingHeader,
        fragment_index: u64,
        session_id: u64
    ) -> Packet {
        routing_header.hop_index += 1;

        Packet {
            pack_type: PacketType::Ack(Ack {
                fragment_index
            }),
            routing_header,
            session_id
        }
    }

    fn build_message(
        mut routing_header: SourceRoutingHeader,
        fragment: Fragment,
        session_id: u64
    ) -> Packet {

        //take the next node and prepare the packet for the next node
        routing_header.hop_index += 1;
        Packet {
            pack_type: PacketType::MsgFragment(fragment),
            routing_header,
            session_id
        }
    }

    fn build_flood_request(
        mut routing_header: SourceRoutingHeader,
        flood_request: FloodRequest,
        session_id: u64
    ) -> Packet {
        Packet {
            pack_type: PacketType::FloodRequest(
                FloodRequest{
                    flood_id: flood_request.flood_id,
                    initiator_id: flood_request.initiator_id,
                    path_trace: flood_request.path_trace.clone()
                }),
            routing_header,
            session_id,
        }
    }

    fn build_flood_response(
        mut routing_header: SourceRoutingHeader,
        flood_id: u64,
        path_trace: Vec<(NodeId, NodeType)>,
        session_id: u64
    ) -> Packet {
        routing_header.hop_index += 1;
        Packet {
            pack_type: PacketType::FloodResponse(
                FloodResponse{
                    flood_id,
                    path_trace
                }),
            routing_header,
            session_id,
        }
    }

    fn forward_packet(
        &self,
        sender: Sender<Packet>,
        packet: Packet,
        packet_succ_event: DroneEvent,
        packet_fail_event: DroneEvent
    ) {

        let _ = match sender.send(packet.clone()){
            Ok(_) =>  self.controller_send.send(packet_succ_event),
            Err(_) =>self.controller_send.send(packet_fail_event)

            ,
        };

    }

    fn handle_packet(&mut self, packet: Packet) {
        println!("Drone {} received a packet of type: {:?}", self.id, packet.pack_type);
        let packet_dropped_event = DroneEvent::PacketDropped(packet.clone());
        let packet_sent_event = DroneEvent::PacketSent(packet.clone());
        let packet_shortcut_event = DroneEvent::ControllerShortcut(packet.clone());
        match packet.pack_type {
          
            //when i receive a NACK, the routing header is reversed, so instead of -1 i perform a +1 to go to the "previous" node
            PacketType::Nack(nack) => {
                
                let next_node = packet.routing_header.hops[packet.routing_header.hop_index + 1];

                if let Some(sender) = self.packet_send.get(&next_node) {

                    // Forward Nack, not initialization
                    let mut routing_header = packet.routing_header.clone();
                    routing_header.hop_index += 1;
                    
                    let nack = Packet {
                        pack_type: PacketType::Nack(nack),
                        routing_header: routing_header.clone(),
                        session_id: packet.session_id
                    };

                    self.forward_packet(sender.clone(), nack, packet_sent_event, packet_shortcut_event);
                    
                }
                else {
                    let _ = self.controller_send.send(packet_dropped_event);
                    println!("ERROREEEEEE")
                }
            },

            PacketType::Ack(ack) => {
                let next_node = packet.routing_header.hops[packet.routing_header.hop_index + 1];

                if let Some(sender) = self.packet_send.get(&next_node) {

                    let ack = Self::build_ack(
                        packet.routing_header.clone(),
                        ack.fragment_index,
                        packet.session_id
                    );

                    self.forward_packet(sender.clone(), ack, packet_sent_event, packet_shortcut_event);

                }
                else {
                    let _ = self.controller_send.send(packet_dropped_event);
                    println!("ERROREEEEEE")
                }
            },

            PacketType::MsgFragment(fragment) => {

                // STEP 1
                // routing_header.hop_index refer to the current node
                // if the hop_index is equal to my id, it mean that the message is for me
                if packet.routing_header.hops[packet.routing_header.hop_index] == self.id {
                    
                    //STEP 2 
                    // increasing the hop index by 1 (to refer the next node)
                    //STEP 3
                    // check the destination, if the destination is equal to the number of hops, it means that this is the last one
                    if packet.routing_header.hop_index + 1 == packet.routing_header.hops.len() {

                        let nack = Self::build_nack(
                            packet.routing_header.clone(),
                            fragment.fragment_index,
                            packet.session_id,
                            NackType::DestinationIsDrone,
                        );
    
                        //send a nack
                        let sender = self.packet_send.get(&packet.routing_header.hops[packet.routing_header.hop_index]).unwrap();
                        let _ = self.controller_send.send(packet_dropped_event); //send the status of the original packet
                        self.forward_packet(sender.clone(), nack.clone(), DroneEvent::PacketSent(nack.clone()), packet_shortcut_event);
                        return 
                    }
                    else {
                        // if i'm not the last node, i try to see if the next node is reachable (is one of my neighbour)
                        let new_packet = Self::build_message(
                            packet.routing_header.clone(),
                            fragment.clone(),
                            packet.session_id
                        );

                        let next_hop = new_packet.routing_header.hops[new_packet.routing_header.hop_index];
                        //if the next node is reachable
                        if let Some(next_node) = self.packet_send.get(&next_hop) {
                            
                            let drop = rand::thread_rng().gen_range(0.0..1.0);
                            //STEP 5
                            if drop <= self.pdr {

                                let nack = Self::build_nack(
                                    packet.routing_header.clone(),
                                    fragment.fragment_index,
                                    packet.session_id,
                                    NackType::Dropped,
                                );

                                //send
                                let sender = self.packet_send.get(&nack.routing_header.hops[nack.routing_header.hop_index]).unwrap();
                                let _ = self.controller_send.send(packet_dropped_event);
                                self.forward_packet(sender.clone(), nack.clone(), DroneEvent::PacketSent(nack.clone()), packet_shortcut_event);
                                return;
                            } else {
                                
                                //forward the packet and send the status, it is not sent, log a drop event
                                self.forward_packet(next_node.clone(), new_packet, packet_sent_event, packet_dropped_event);
                                return;
                            }

                        }
                        else {
                            // STEP 4 : if it is not in my neighbour, send a nack of type error in routing

                            let nack = Self::build_nack(
                                packet.routing_header.clone(),
                                fragment.fragment_index,
                                packet.session_id,
                                NackType::ErrorInRouting(next_hop),
                            );

                            //send
                            let sender = self.packet_send.get(&nack.routing_header.hops[nack.routing_header.hop_index]).unwrap();
                            let _ = self.controller_send.send(packet_dropped_event); //send the status of the original packet to the controller
                            self.forward_packet(sender.clone(), nack.clone(), DroneEvent::PacketSent(nack.clone()), packet_shortcut_event);
                            return;
                        }
                    }
                }
                else {
                    let nack = Self::build_nack(
                        packet.routing_header.clone(),
                        fragment.fragment_index,
                        packet.session_id,
                        NackType::UnexpectedRecipient(self.id),
                    );

                    //send
                    let sender = self.packet_send.get(&nack.routing_header.hops[nack.routing_header.hop_index]).unwrap();
                    let _ = self.controller_send.send(packet_dropped_event);
                    self.forward_packet(sender.clone(), nack.clone(), DroneEvent::PacketSent(nack.clone()), packet_shortcut_event);
                    return;
                }
                
            },

            PacketType::FloodRequest(mut flood_request) => {

                // initialize flood response
                //if this drone has already received this flood request, send a flood response and do not forward it
                if self.flood_session.contains(&(flood_request.flood_id, flood_request.initiator_id)) ||
                    self.packet_send.len() == 1 {

                    if !flood_request.path_trace.contains(&(self.id, NodeType::Drone)){
                        flood_request.path_trace.push((self.id, NodeType::Drone))
                    }

                    let mut route: Vec<_> = flood_request.path_trace.iter().map(|(id, _)| *id).collect();
                    route.reverse();

                    let mut routing_header = SourceRoutingHeader{
                        hop_index: 0,
                        hops: route
                    };

                    let new_flood_response = Self::build_flood_response(
                        routing_header.clone(),
                        flood_request.flood_id,
                        flood_request.path_trace.clone(),
                        packet.session_id
                    );

                    let sender = self.packet_send
                        .get(&new_flood_response.routing_header.hops[new_flood_response.routing_header.hop_index])
                        .unwrap();

                    self.forward_packet(sender.clone(), new_flood_response, packet_sent_event, packet_shortcut_event);


                } else { // forward flood request
                    self.flood_session.insert((flood_request.flood_id, flood_request.initiator_id));
                    let sender_node_id = flood_request.path_trace.len() - 1;
                    flood_request.path_trace.push((self.id, NodeType::Drone));
                    let new_flood_request = Self::build_flood_request(
                        packet.routing_header.clone(),
                        flood_request.clone(),
                        packet.session_id
                    );
                    for neighbour in self.packet_send.clone() {
                        if neighbour.0 != flood_request.path_trace[sender_node_id].0 {

                            // this should be right
                            let sender = neighbour.1.clone();
                            self.forward_packet(sender, new_flood_request.clone(), packet_sent_event.clone(), packet_dropped_event.clone());
                        }
                    }
                }
            },

            PacketType::FloodResponse(flood_response) => {
                let next_node = packet.routing_header.hops[packet.routing_header.hop_index + 1];

                if let Some(sender) = self.packet_send.get(&next_node) {

                    let flood_response = Self::build_flood_response(
                        packet.routing_header.clone(),
                        flood_response.flood_id,
                        flood_response.path_trace.clone(),
                        packet.session_id
                    );

                    self.forward_packet(sender.clone(), flood_response, packet_sent_event, packet_shortcut_event);

                }
                else {
                    let _ = self.controller_send.send(packet_shortcut_event);
                    println!("ERROREEEEEE")
                }

            },
        }
        // TODO: test flood response, request
    }

    fn handle_packet_crash(&mut self, packet: Packet) {

        let packet_dropped_event = DroneEvent::PacketDropped(packet.clone());
        let packet_shortcut_event = DroneEvent::ControllerShortcut(packet.clone());
        match packet.pack_type {

            PacketType::MsgFragment(fragment) => {
                let nack = Self::build_nack(
                    packet.routing_header.clone(),
                    fragment.fragment_index,
                    packet.session_id,
                    NackType::ErrorInRouting(self.id),
                );

                //send
                let sender = self.packet_send
                    .get(&nack.routing_header.hops[nack.routing_header.hop_index])
                    .unwrap();
                let _ = self.controller_send.send(packet_dropped_event); //send the status of the original packet to the controller
                self.forward_packet(
                    sender.clone(),
                    nack.clone(),
                    DroneEvent::PacketSent(nack.clone()),
                    packet_shortcut_event
                );
            },

            PacketType::FloodRequest(flood_request) => {
                let nack = Self::build_nack(
                    packet.routing_header.clone(),
                    flood_request.flood_id,
                    packet.session_id,
                    NackType::ErrorInRouting(self.id),
                );

                //send
                let sender = self.packet_send
                    .get(&nack.routing_header.hops[nack.routing_header.hop_index])
                    .unwrap();
                let _ = self.controller_send.send(packet_dropped_event); //send the status of the original packet to the controller
                self.forward_packet(
                    sender.clone(),
                    nack.clone(),
                    DroneEvent::PacketSent(nack),
                    packet_shortcut_event
                );
            },

            _ => self.handle_packet(packet)

        }
    }

    fn handle_command(&mut self, command: DroneCommand) -> bool {
        match command {
            DroneCommand::AddSender(node_id, sender) => {
                if !self.packet_send.contains_key(&node_id) {
                    self.packet_send.insert(node_id, sender);
                }
                false
            },

            DroneCommand::SetPacketDropRate(pdr) => {
                if pdr >= 0.0 && pdr <= 1.0 {
                    self.pdr = pdr;
                }
                false
            },

            DroneCommand::Crash => true,

            DroneCommand::RemoveSender(node_id) => {
                if let Some(_removed_sender) = self.packet_send.remove(&node_id) {
                    println!("Removed {} from drone {}", node_id, self.id);
                }
                else {
                    println!("{} not found in drone {}", node_id, self.id);
                }
                false
            },
        }
    }

    fn drop_senders(&mut self) {
        for sender in self.packet_send.clone() {
            self.packet_send.remove(&sender.0);
        }
    }
}

struct SimulationController {
    drones: HashMap<NodeId, Sender<DroneCommand>>,
    node_event_recv: Receiver<DroneEvent>
}

impl SimulationController {
    fn crash_all(&mut self) {
        for (_, sender) in self.drones.iter() {
            sender.send(DroneCommand::Crash).unwrap();
        }
    }

    fn crash_by_id(&mut self, drone_id: NodeId) {
        for (id, sender) in self.drones.iter() {

            if drone_id == *id {
                sender.send(DroneCommand::Crash).unwrap();
                break;
            }
        }
    }

    fn remove_sender(&mut self, drone_id: NodeId, sender_id: NodeId) {
        if let Some(sender) = self.drones.get(&drone_id) {
            sender.send(DroneCommand::RemoveSender(sender_id)).unwrap();
        }
    }

    fn add_sender(&mut self, drone_id: NodeId, sender_id: NodeId, new_sender: Sender<Packet>) {
        if let Some(sender) = self.drones.get(&drone_id) {
            sender.send(DroneCommand::AddSender(sender_id, new_sender)).unwrap();
        }
    }

    fn set_packet_drop_rate(&mut self, drone_id: NodeId, pdr: f32) {
        if let Some(sender) = self.drones.get(&drone_id) {
            sender.send(DroneCommand::SetPacketDropRate(pdr)).unwrap();
        }
    }
}

fn parse_config(file: &str) -> Config {
    let file_str = fs::read_to_string(file).unwrap();
    toml::from_str(&file_str).unwrap()
}

fn main() {

    let (_drone_send, _drone_recv) = unbounded::<Packet>();
    let (_controller_send, controller_recv) = unbounded();
    //while let Ok(_) = controller_recv.try_recv() {}

    let (packet_send, packet_recv) = unbounded();
    //while let Ok(_) = packet_recv.try_recv() {}


    let mut drone = RustDoIt::new(
        1,
        unbounded().0,
        controller_recv,
        packet_recv,
        HashMap::new(),
        0.0
    );

    let drone_thread = thread::spawn(move || {
        drone.run();
    });

    let fragment = Fragment {
        fragment_index: 0,
        total_n_fragments: 1,
        length: 1,
        data: [0; 128],
    };

    let routing_header = SourceRoutingHeader {
        hop_index: 1,
        hops: vec![0,1,2,3],
    };

    let packet = Packet {
        pack_type: PacketType::MsgFragment(fragment),
        routing_header,
        session_id: 0,
    };

    match packet_send.send(packet) {
        Ok(()) => println!("Ok"),
        _ => println!("Error")
    }

    drone_thread.join().unwrap();
}



#[cfg(test)]
mod tests {
    use wg_2024::packet::Fragment;
    use wg_2024::packet::PacketType::{FloodRequest, FloodResponse};
    use super::*;
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

    fn create_custom_fragment(
        fragment_index: u64,
        total_n_fragments: u64,
        length: u8,
        data: [u8; 128]
    ) -> Fragment {
        Fragment {
            fragment_index,
            total_n_fragments,
            length,
            data
        }
    }

    fn create_custom_routing_header(
        hop_index: usize,
        hops: Vec<NodeId>
    ) -> SourceRoutingHeader {
        SourceRoutingHeader {
            hop_index,
            hops
        }
    }

    fn create_custom_packet(
        source_routing_header: SourceRoutingHeader,
        packet_type: PacketType,
        session_id: u64
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
    pub fn generic_packet_forward(){
        let (d_send, d_recv) = unbounded();
        let (d2_send, d2_recv) = unbounded::<Packet>();
        let (_d_command_send, d_command_recv) = unbounded();
        let (d_event_send,d_event_recv) = unbounded();
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
        let packet_sent_event = DroneEvent::PacketSent(msg.clone());

        // "Client" sends packet to d
        d_send.send(msg.clone()).unwrap();
        msg.routing_header.hop_index += 1;

        // d2 receives packet from d1
        let packet_received = d2_recv.recv().unwrap();
        let event_log = d_event_recv.recv().unwrap();

        assert_eq!(packet_received, msg);
        assert_eq!(event_log,  packet_sent_event);
    }


    #[test]
    //#[cfg(feature = "partial_eq")]
    /// Test forward functionality of a nack for a drone
    pub fn generic_nack_forward() {
        let (d1_send, d1_recv) = unbounded();
        let (d2_send, d2_recv) = unbounded::<Packet>();
        let (_d_command_send, d_command_recv) = unbounded();
        let (d_event_send,d_event_recv) = unbounded();
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
        let packet_sent_event = DroneEvent::PacketSent(nack.clone());
        nack.routing_header.hop_index += 1;
        let packet_received = d2_recv.recv().unwrap();

        assert_eq!(packet_received, nack);
        assert_eq!(event,packet_sent_event);

    }

    #[test]
    //#[cfg(feature = "partial_eq")]
    /// Checks if the packet is dropped by a drone and a Nack is sent back. The drone MUST have 100% packet drop rate, otherwise the test will fail sometimes.
    pub fn generic_fragment_drop() {
        // Client 1
        let (c_send, c_recv) = unbounded();
        // Drone 11
        let (d_send, d_recv) = unbounded();
        // SC commands
        let (_d_command_send, d_command_recv) = unbounded();
        let (d_event_send,d_event_recv) = unbounded();


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
            fragment_index: 1,
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

        let packet_sent_event = DroneEvent::PacketSent(nack_packet.clone());
        let packet_drop_event = DroneEvent::PacketDropped(msg.clone());
        let packet_dropped = d_event_recv.recv().unwrap();
        let nack_sent = d_event_recv.recv().unwrap();

        assert_eq!(packet_sent_event,nack_sent);
        assert_eq!(packet_drop_event,packet_dropped);
        assert_eq!(c_recv.recv().unwrap(), nack_packet);
    }

    #[test]
    /// Checks if the packet is dropped by the second drone and a Nack is sent back. The first drone must have 0% PDR and the second one 100% PDR, otherwise the test will fail sometimes.
    pub fn generic_chain_fragment_drop() {
        // Client 1 channels
        let (c_send, c_recv) = unbounded();
        // Server 21 channels
        let (s_send, s_recv) = unbounded();
        // Drone 11
        let (d_send, d_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (_d_command_send, d_command_recv) = unbounded();

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
            1.0,
        );

        // Spawn the drone's run method in a separate thread
        thread::spawn(move || {
            drone1.run();
        });
        thread::spawn(move || {
            drone2.run();
        });

        let msg = create_sample_packet();

        // "Client" sends packet to the drone1
        d_send.send(msg.clone()).unwrap();

        // Client receive an NACK originated from drone2
        let packet_true = Packet {
            pack_type: PacketType::Nack(Nack {
                fragment_index: 1,
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

        // let test1_got = format!("TEST 4.0: {:?}", c_recv.recv().unwrap());
        // let test1_true = format!("TEST 4.0: {:?}", packet_true);
        // assert_eq!(test1_got, test1_true, "TEST 4.0 PASSED --> {}", test1_got == test1_true);
        // println!("TEST 4.0 PASSED --> {}", test1_got == test1_true);
        // if test1_got != test1_true {
        //    println!("GOT {}", test1_got);
        //    println!("EXPECTED {}", test1_true);
        //    println!("TEST 4.0 FAILED");
        //}
    }

    #[test]
    /// Test forward functionality of a generic packet for a chain of drones
    pub fn generic_chain_fragment_forward() {
        // Client 1 channels
        let (c_send, c_recv) = unbounded();
        // Server 21 channels
        let (s_send, s_recv) = unbounded();
        // Drone 11
        let (d_send, d_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (_d_command_send, d_command_recv) = unbounded();

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
        assert_eq!(packet_true,packet_got);

        // let test1_got = format!("TEST 5.0: {:?}", s_recv.recv().unwrap());
        // let test1_true = format!("TEST 5.0: {:?}", packet_true);
        //
        // assert_eq!(test1_got, test1_true, "TEST 5.0 PASSED --> {}", test1_got == test1_true);
        /*println!("TEST 5.0 PASSED --> {}", test1_got == test1_true);
        if test1_got != test1_true {
            println!("GOT {}", test1_got);
            println!("EXPECTED {}", test1_true);
            println!("TEST 5.0 FAILED");
        }*/
    }

    #[test]
    /// Test crash all drones
    fn test_crash_all() {

        let config = parse_config("src/config.toml");

        // Da ad ogni drone un canale per scambiare pacchetti (unbounded)
        let mut packet_channels = HashMap::new();
        for drone in config.drone.iter() {
            packet_channels.insert(drone.id, unbounded());
        }

        // Da ad ogni client un canale per scambiare pacchetti (unbounded)
        for client in config.client.iter() {
            packet_channels.insert(client.id, unbounded());
        }

        // Da ad ogni server un canale per scambiare pacchetti (unbounded)
        for server in config.server.iter() {
            packet_channels.insert(server.id, unbounded());
        }

        let mut handles = Vec::new();

        let mut drones = HashMap::new();
        let (node_event_send, node_event_recv) = unbounded();

        for drone in config.drone.into_iter() {

            // Da al controller un canale per comunicare con ogni drone
            let (controller_drone_send, controller_drone_recv) = unbounded();
            drones.insert(drone.id, controller_drone_send);

            // Canale drone -> controller per i log
            let node_event_send = node_event_send.clone();

            // Clona il receiver di ogni drone
            let packet_recv = packet_channels[&drone.id].1.clone();

            // Prende tutti i sender verso ogni drone
            let packet_send: HashMap<NodeId, Sender<Packet>> = drone
                .connected_node_ids
                .into_iter()
                .map(|id| (id, packet_channels[&id].0.clone()))
                .collect();

            // Create the drone
            let mut new_drone = RustDoIt::new(
                drone.id,
                node_event_send, // Uso il canale send per inviare al controller i log
                controller_drone_recv, // Do al drone il canale per ricevere i comandi del controller
                packet_recv, // Do al drone il receiver per ricevere i pacchetti
                packet_send, // Do al drone una hashmap per comunicare a tutti i droni COLLEGATI
                drone.pdr
            );
            handles.push(thread::spawn(move || {
                // Execute the drone
                new_drone.run();
            }));
        }

        // Crea il controller
        let mut controller = SimulationController {
            drones,
            node_event_recv
        };

        controller.crash_all();

        // Waits until all threads end
        while let Some(handle) = handles.pop() {
            handle.join().unwrap();
        }

        println!("TEST 6.0 PASSED --> true")
    }

    #[test]
    /// Test the forward of a flood request coming from drone1, forwarded to drone2 and drone3
    pub fn flood_request_forward() {
        // Client 1 channels
        let (c_send, c_recv) = unbounded();
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

        // Drone 11
        let neighbours11 = HashMap::from([(12, d12_send.clone()), (13, d13_send.clone()), (1, c_send.clone())]);
        let mut drone1 = RustDoIt::new(
            11,
            unbounded().0,
            d_command_recv.clone(),
            d11_recv.clone(),
            neighbours11,
            0.0,
        );
        // Drone 12
        let neighbours12 = HashMap::from([(11, d_send11.clone()), (21, s_send.clone())]);
        let mut drone2 = RustDoIt::new(
            12,
            unbounded().0,
            d_command_recv.clone(),
            d12_recv.clone(),
            neighbours12,
            0.0,
        );
        let neighbours13 = HashMap::from([(11, d_send11.clone()), (21, s_send.clone())]);
        let mut drone3 = RustDoIt::new(
            13,
            unbounded().0,
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

        let srh = create_custom_routing_header( // useless in the flood_request
            0,
            vec![],
        );

        let flood_request = create_custom_packet(
            srh,
            FloodRequest(
                wg_2024::packet::FloodRequest{
                flood_id: 0,
                initiator_id: 1,
                path_trace: vec![(1, NodeType::Client)],
            }),
            0,
        );

        // Client sends packet to the drone1
        d_send11.send(flood_request.clone()).unwrap();

        // Server receives a packet with the same content of msg but with hop_index+2
        let mut packet_true_2 = flood_request.clone();
        packet_true_2.pack_type = FloodRequest(
            wg_2024::packet::FloodRequest{
                flood_id: 0,
                initiator_id: 1,
                path_trace: vec![(1, NodeType::Client), (11, NodeType::Drone), (12, NodeType::Drone)],
            }
        );

        let mut packet_true_3 = flood_request.clone();
        packet_true_3.pack_type = FloodRequest(
            wg_2024::packet::FloodRequest{
                flood_id: 0,
                initiator_id: 1,
                path_trace: vec![(1, NodeType::Client), (11, NodeType::Drone), (13, NodeType::Drone)],
            }
        );
        let packet_got = s_recv.recv().unwrap();
        assert!(packet_got == packet_true_2 || packet_got == packet_true_3);
        let packet_got = s_recv.recv().unwrap();
        assert!(packet_got == packet_true_2 || packet_got == packet_true_3);

        //let test2_got = format!("TEST 7.0: {:?}", s_recv.recv().unwrap());
        //let test2_true = format!("TEST 7.0: {:?}", packet_true_2);
        //let test3_got = format!("TEST 7.1: {:?}", s_recv.recv().unwrap());
        //let test3_true = format!("TEST 7.1: {:?}", packet_true_3);
        //assert!(test2_got == test2_true || test2_got == test3_true, "TEST 7.0 PASSED --> {}", test2_got == test2_true || test2_got == test3_true);
        //assert!(test3_got == test2_true || test3_got == test3_true, "TEST 7.1 PASSED --> {}", test3_got == test2_true || test3_got == test3_true);assert_eq!(test3_got, test3_true, "TEST 7.1 PASSED --> {}", test3_got == test3_true);
        /*println!("TEST 7.0 PASSED --> {}", test2_got == test2_true || test2_got == test3_true);
        println!("TEST 7.1 PASSED --> {}", test3_got == test2_true || test3_got == test3_true);
        if !(test2_got == test2_true || test2_got == test3_true) {
            println!("GOT {}", test2_got);
            println!("EXPECTED {}", test2_true);
            println!("TEST 7.0 FAILED");
        }
        if !(test3_got == test2_true || test3_got == test3_true) {
            println!("GOT {}", test3_got);
            println!("EXPECTED {}", test3_true);
            println!("TEST 7.1 FAILED");
        }*/
    }

    #[test]
    /// Test the forward of a flood response coming from drone2, forwarded to drone1, forwarded again
    pub fn flood_response_forward() {
        // Client 1 channels
        let (c_send, c_recv) = unbounded();
        // Server 21 channels
        let (s_send, s_recv) = unbounded();
        // Drone 11
        let (d_send, d_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (_d_command_send, d_command_recv) = unbounded();

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

        let srh = create_custom_routing_header( // to use in flood_response
            1,
            vec![21, 12, 11, 1],
        );


        let flood_response = create_custom_packet(
            srh,
            FloodResponse(
                wg_2024::packet::FloodResponse{
                    flood_id: 0,
                    path_trace: vec![(1, NodeType::Client), (11, NodeType::Drone), (12, NodeType::Drone), (21, NodeType::Server)],
                }
            ),
            0,
        );

        // Client sends packet to the drone
        d12_send.send(flood_response.clone()).unwrap();

        // Server receives a packet with the same content of msg but with hop_index+2
        let mut packet_true = flood_response.clone();
        packet_true.routing_header.hop_index += 2;

        let test1_got = format!("TEST 8.0: {:?}", c_recv.recv().unwrap());
        let test1_true = format!("TEST 8.0: {:?}", packet_true);
        assert_eq!(test1_got, test1_true, "TEST 8.0 PASSED --> {}", test1_got == test1_true);
        /*println!("TEST 8.0 PASSED --> {}", test1_got == test1_true);
        if test1_got != test1_true {
            println!("GOT {}", test1_got);
            println!("EXPECTED {}", test1_true);
            println!("TEST 8.0 FAILED");
        }*/
    }

    #[test]
    /// Test the generation of a flood response due to an isolated drone (only neighbour the one who sent the flood request)
    pub fn flood_response_isolation() {
        // Client 1 channels
        let (c_send, c_recv) = unbounded();
        // Drone 11
        let (d_send, d_recv) = unbounded();
        // Drone 12
        let (d12_send, d12_recv) = unbounded();
        // SC - needed to not make the drone crash
        let (_d_command_send, d_command_recv) = unbounded();

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


        let srh = create_custom_routing_header( // to use in flood_response
            0,
            vec![],
        );

        let flood_request = create_custom_packet(
            srh,
            FloodRequest(
                wg_2024::packet::FloodRequest{
                    flood_id: 0,
                    initiator_id: 1,
                    path_trace: vec![(1, NodeType::Client)],
                }),
            0,
        );

        // Client sends packet to the drone
        d_send.send(flood_request.clone()).unwrap();

        let srh = create_custom_routing_header(
            2,
            vec![12, 11, 1],
        );
        let packet_true = create_custom_packet(
            srh,
            FloodResponse(
                wg_2024::packet::FloodResponse{
                    flood_id: 0,
                    path_trace: vec![(1, NodeType::Client), (11, NodeType::Drone), (12, NodeType::Drone)],
                }
            ),
            0,
        );

        let test1_got = format!("TEST 9.0: {:?}", c_recv.recv().unwrap());
        let test1_true = format!("TEST 9.0: {:?}", packet_true);
        assert_eq!(test1_true, test1_got, "TEST 9.0 PASSED --> {}", test1_got == test1_true);
        /*println!("TEST 9.0 PASSED --> {}", test1_got == test1_true);
        if test1_got != test1_true {
            println!("GOT {}", test1_got);
            println!("EXPECTED {}", test1_true);
            println!("TEST 9.0 FAILED");
        }*/
    }

    #[test]
    /// Test the generation of a flood response due to an already visited hop
    pub fn flood_response_visited() {}

    //pub fn generic_crash_command() {}
}
