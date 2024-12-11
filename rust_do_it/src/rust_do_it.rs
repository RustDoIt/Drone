extern crate wg_2024;

use log::{info, warn, error, debug};
use rand::Rng;
use wg_2024::drone::{Drone};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::NodeId;
use wg_2024::packet::{Nack, NackType, Packet, PacketType, NodeType, FloodRequest, FloodResponse};
use wg_2024::network::SourceRoutingHeader;
use crossbeam_channel::select_biased;
use crossbeam_channel::{Receiver, Sender};
use wg_2024::packet::Fragment;
use std::collections::{HashMap, HashSet};



#[derive(Debug)]
pub struct RustDoIt {
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
        loop {
            // Use select_biased to handle incoming commands and packets in normal operation
            select_biased! {
                recv(self.controller_recv) -> command => {
                    if let Ok(command) = command {
                        debug!("Drone {} received command {:?}", self.id, command);
                        if let DroneCommand::Crash = command {
                            self.handle_command(command);
                            return;
                        }
                        self.handle_command(command);
                    }
                },
                recv(self.packet_recv) -> packet => {
                    if let Ok(packet) = packet {
                        debug!("Drone {} received packet {:?}", self.id, packet);
                        self.handle_packet(packet);
                    }
                }
            }
        }

    }
}

impl RustDoIt {

    /// This function handles the commands sent by the controller
    fn handle_command(&mut self, command: DroneCommand) {
        match command {
            DroneCommand::AddSender(node_id, sender) => {
                if !self.packet_send.contains_key(&node_id) {
                    self.packet_send.insert(node_id, sender);
                    debug!("Drone {} added sender {}", self.id, node_id);
                }
            },

            DroneCommand::SetPacketDropRate(pdr) => {
                if pdr >= 0.0 && pdr <= 1.0 {
                    self.pdr = pdr;
                    debug!("Drone {} set PDR to {}", self.id, self.pdr);
                }
            },

            DroneCommand::Crash => {
                while let Ok(packet) = self.packet_recv.try_recv() {
                    self.handle_packet_crash(packet);
                    debug!("Drone {} crashed", self.id);
                };
                return;
            },

            DroneCommand::RemoveSender(node_id) => {
                if let Some(_removed_sender) = self.packet_send.remove(&node_id) {
                    debug!("Drone {} removed sender {}", self.id, node_id);
                }
            },
        }
    }

    /// This function handles the packet
    fn handle_packet(&mut self, packet: Packet) {

        match packet.pack_type {
            PacketType::Ack(ack) => self.handle_ack(
                ack.fragment_index,
                packet.routing_header,
                packet.session_id
            ),
            PacketType::Nack(nack) => self.handle_nack(
                nack,
                packet.routing_header,
                packet.session_id
            ),
            PacketType::FloodRequest(flood_request) => self.handle_flood_request(
                flood_request,
                packet.routing_header,
                packet.session_id
            ),
            PacketType::FloodResponse(flood_response) => self.handle_flood_response(
                flood_response,
                packet.routing_header,
                packet.session_id
            ),
            PacketType::MsgFragment(fragment) => self.handle_fragment(
                packet.routing_header,
                fragment, packet.session_id
            ),
        }
    }

    /// This function handles the packet in case of a crash
    fn handle_packet_crash(&mut self, packet: Packet) {
        match packet.pack_type {
            PacketType::MsgFragment(_) => {
                self.generate_nack(
                    NackType::ErrorInRouting(self.id),
                    packet.routing_header.clone(),
                    packet.session_id
                );
                if self.controller_send.send(DroneEvent::PacketDropped(packet)).is_err() {
                    error!("Drone {} could not send packet to controller", self.id);
                }
            },

            PacketType::Ack(ack) => {
                self.handle_ack(
                    ack.fragment_index,
                    packet.routing_header,
                    packet.session_id
                );
            }
            PacketType::Nack(nack) => self.handle_nack(
                nack,
                packet.routing_header,
                packet.session_id
            ),

            PacketType::FloodRequest(_) => {
                self.generate_nack(
                    NackType::ErrorInRouting(self.id),
                    packet.routing_header.clone(),
                    packet.session_id
                );
                let _ = self.controller_send.send(DroneEvent::PacketDropped(packet));
            },

            PacketType::FloodResponse(flood_response) => self.handle_flood_response(
                flood_response,
                packet.routing_header,
                packet.session_id
            )
        }
    }

    /// This function forward the nack to the next hop
    fn handle_nack(&self, nack: Nack, mut srh: SourceRoutingHeader, session_id: u64) {
        // step 1: check if the drone is the correct recipient
        if !self.is_correct_recipient(&srh, session_id) {
            return;
        }

        // step 3: check if the destination is legit
        match &srh.next_hop() {
            None => {
                self.generate_nack(NackType::DestinationIsDrone, srh, session_id);
                return;
            },
            Some(next_hop) => {

                // step 2: increase the hop index
                srh.increase_hop_index();

                let new_nack = Packet::new_nack(
                    srh,
                    session_id,
                    nack,
                );

                self.forward_packet(new_nack, next_hop);
            }
        }
    }

    /// This function forward the ack to the next hop
    fn handle_ack(&self, fragment_index: u64, mut srh: SourceRoutingHeader, session_id: u64) {
        if !self.is_correct_recipient(&srh, session_id) {
            return;
        }

        match &srh.next_hop() {
            None => {
                self.generate_nack(NackType::DestinationIsDrone, srh, session_id);
                return;
            },
            Some(next_hop) => {
                srh.increase_hop_index();

                if !srh.valid_hop_index() {
                    self.generate_nack(NackType::DestinationIsDrone, srh, session_id);
                    return;
                }

                let new_ack = Packet::new_ack(
                    srh,
                    session_id,
                    fragment_index,
                );

                self.forward_packet(new_ack, next_hop);
            }
        }


    }

    /// This function generates a nack and sends it to the next hop
    fn generate_nack(&self, nack_type: NackType, mut srh: SourceRoutingHeader, session_id: u64) {
        let nack = Nack {
            fragment_index: 0,
            nack_type,
        };

        match nack_type {
            NackType::ErrorInRouting(_) => {
                // reverse the trace
                srh = srh.sub_route(0..srh.hop_index).unwrap();
                srh.hops.reverse();
                srh.hop_index = 1;
            },
            NackType::UnexpectedRecipient(_) => {
                // reverse the trace
                srh = srh.sub_route(0..=srh.hop_index).unwrap();
                srh.hops.pop();
                srh.hops.push(self.id);
                srh.hops.reverse();
                srh.hop_index = 1;
            },
            _ => {
                // reverse the trace
                srh = srh.sub_route(0..=srh.hop_index).unwrap();
                srh.hops.reverse();
                srh.hop_index = 1;
            }
        }

        let new_nack = Packet::new_nack(
            srh,
            session_id,
            nack,
        );
        let next_hop = &new_nack.routing_header.current_hop().unwrap();

        // send nack to next hop, if send fails, send to controller
        self.forward_packet(new_nack, next_hop);

    }

    /// This function handles the fragment and forwards it to the next hop
    fn handle_fragment(&self, mut srh: SourceRoutingHeader, fragment: Fragment, session_id: u64) {
        if !self.is_correct_recipient(&srh, session_id) {
            return;
        }

        match &srh.next_hop() {
            None => {
                self.generate_nack(NackType::DestinationIsDrone, srh, session_id);
                return;
            },
            Some(next_hop) => {
                let drop = rand::thread_rng().gen_range(0.0..1.0);
                if drop <= self.pdr {

                    let dropped_fragment = Packet::new_fragment(
                        srh,
                        session_id,
                        fragment,
                    );
                    let _ = self.controller_send.send(
                        DroneEvent::PacketDropped(dropped_fragment.clone())
                    );
                    self.generate_nack(
                        NackType::Dropped,
                        dropped_fragment.routing_header,
                        session_id
                    );
                    return;
                }

                srh.increase_hop_index();

                if !srh.valid_hop_index() {
                    self.generate_nack(NackType::DestinationIsDrone, srh, session_id);
                    return;
                }

                let new_fragment = Packet::new_fragment(
                    srh,
                    session_id,
                    fragment,
                );

                self.forward_packet(new_fragment, next_hop);
            }
        }
    }

    /// This function handles the flood request and forwards it to the next hop
    fn handle_flood_request(
        &mut self,
        mut flood_request: FloodRequest,
        srh: SourceRoutingHeader,
        session_id: u64
    ) {


        let prev_hop = flood_request.clone().path_trace.last().unwrap().0;

        if !flood_request.path_trace.contains(&(self.id, NodeType::Drone)) {
            flood_request.path_trace.push((self.id, NodeType::Drone));
        }

        let flood_session = (flood_request.flood_id, flood_request.initiator_id);
        if self.flood_session.contains(&flood_session) {
            self.generate_flood_response(flood_request, session_id);
            return;
        }
        self.flood_session.insert(flood_session);

        if self.packet_send.len() == 1 && self.packet_send.contains_key(&prev_hop){
            self.generate_flood_response(flood_request, session_id);
            return;
        }

        let new_flood_request = Packet::new_flood_request(
            srh,
            session_id,
            flood_request,
        );

        for neighbor in &self.packet_send {
            if *neighbor.0 != prev_hop {
                let result = match neighbor.1.send(new_flood_request.clone()) {
                    Ok(_) => self.controller_send.send(DroneEvent::PacketSent(new_flood_request.clone())),
                    Err(_) => self.controller_send.send(DroneEvent::ControllerShortcut(new_flood_request.clone())),
                };

                if result.is_err() {
                    error!("Drone {} could not send packet to controller", self.id);
                }
            }
        }
    }

    /// This function generates a flood response and sends it to the next hop
    fn generate_flood_response(&self, flood_request: FloodRequest, session_id: u64) {
        let mut route: Vec<_> = flood_request.path_trace.iter().map(|(id, _)| *id).collect();
        route.reverse();

        let flood_response = FloodResponse {
            flood_id: flood_request.flood_id,
            path_trace: flood_request.path_trace,
        };

        let mut srh = SourceRoutingHeader::new(route, 0);
        match &srh.next_hop() {
            None => {
                self.generate_nack(NackType::DestinationIsDrone, srh, session_id);
                return;
            },
            Some(next_hop) => {
                srh.increase_hop_index();
                let new_flood_response = Packet::new_flood_response(
                    srh,
                    session_id,
                    flood_response
                );
                self.forward_packet(new_flood_response, next_hop);
            }
        }
    }

    /// This function handles the flood response and forwards it to the next hop
    fn handle_flood_response(
            &self,
            flood_response: FloodResponse,
            mut srh: SourceRoutingHeader,
            session_id: u64,
        ) {

        if !self.is_correct_recipient(&srh, session_id) {
            return;
        }

        match &srh.next_hop() {
            None => {
                self.generate_nack(NackType::DestinationIsDrone, srh, session_id);
                return;
            }
            Some(next_hop) => {
                srh.increase_hop_index();
                let flood_response_packet = Packet::new_flood_response(
                    srh,
                    session_id,
                    flood_response
                );
                self.forward_packet(flood_response_packet, next_hop);
            }
        }
    }

    fn is_correct_recipient(&self, srh: &SourceRoutingHeader, session_id: u64) -> bool {
        let current_hop = srh.current_hop();
        if current_hop.is_some() && current_hop != Some(self.id) {
            self.generate_nack(
                NackType::UnexpectedRecipient(current_hop.unwrap()),
                srh.clone(),
                session_id
            );
            false
        } else {
            if current_hop.is_none() {
                error!("Drone {} received packet with empty route", self.id);
                self.generate_nack(
                    NackType::UnexpectedRecipient(current_hop.unwrap()),
                    srh.clone(),
                    session_id
                );
                return false;
            }
            true
        }
    }

    fn forward_packet(
        &self,
        packet: Packet,
        next_hop: &NodeId,
    ) {
        // step 4: Identify the sender and check if is valid
        match self.packet_send.get(next_hop) {
            Some(sender) => {
                // step 5: send the packet to the next hop
                // if the send() is successful, send an event to the controller
                if sender.send(packet.clone()).is_ok() {
                    // if the send fails, log an error
                    if self.controller_send.send(DroneEvent::PacketSent(packet)).is_err() {
                        error!("Drone {} could not send packet to controller", self.id);
                    }
                } else {
                    let result = {
                        if let PacketType::MsgFragment(_) = packet.pack_type {
                            self.controller_send.send(DroneEvent::PacketDropped(packet))
                        } else {
                            self.controller_send.send(DroneEvent::ControllerShortcut(packet))
                        }
                    };

                    if result.is_err() {
                        error!("Drone {} could not send packet to controller", self.id);
                    }
                }
            },
            None => {

                // step 4.1: if the next hop is not in the list of neighbours, generate a nack with
                // NackType::ErrorInRouting and send it back to the source
                match packet.pack_type {
                    PacketType::MsgFragment(_) => {
                        self.generate_nack(
                            NackType::ErrorInRouting(*next_hop),
                            packet.routing_header,
                            packet.session_id
                        );
                    },
                    _ => {
                        if self.controller_send.send(DroneEvent::ControllerShortcut(packet)).is_err() {
                            error!("Drone {} could not send packet to controller", self.id);
                        }
                    }
                }
            }
        }
    }
}
