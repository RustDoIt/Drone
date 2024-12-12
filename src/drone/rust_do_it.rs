extern crate wg_2024;

use super::RustDoIt;
use crossbeam_channel::select_biased;
use crossbeam_channel::{Receiver, Sender};
use log::{debug, error, info, warn};
use rand::Rng;
use std::collections::{HashMap, HashSet};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::drone::Drone;
use wg_2024::network::NodeId;
use wg_2024::network::SourceRoutingHeader;
use wg_2024::packet::{FloodRequest, FloodResponse, Nack, NackType, NodeType, Packet, PacketType};

impl RustDoIt {
    pub fn handle_command(&mut self, command: DroneCommand) {
        /// This function handles the command received from the controller
        /// It checks the command type and takes the appropriate actions
        /// to handle the command
        /// ### Parameters:
        /// - `command`: The command to be handled
        match command {
            DroneCommand::AddSender(node_id, sender) => {
                if !self.packet_send.contains_key(&node_id) {
                    self.packet_send.insert(node_id, sender);
                    debug!("Drone {} added sender {}", self.id, node_id);
                }
            }

            DroneCommand::SetPacketDropRate(pdr) => {
                if pdr >= 0.0 && pdr <= 1.0 {
                    self.pdr = pdr;
                    debug!("Drone {} set PDR to {}", self.id, self.pdr);
                } else {
                    warn!(
                        "Drone {} could not set PDR to {}, value must be between 0.0 and 1.0",
                        self.id, pdr
                    );
                }
            }

            DroneCommand::Crash => {
                while let Ok(packet) = self.packet_recv.try_recv() {
                    self.handle_packet_crash(packet);
                    debug!("Drone {} crashed", self.id);
                }
                return;
            }

            DroneCommand::RemoveSender(node_id) => {
                if let Some(_removed_sender) = self.packet_send.remove(&node_id) {
                    debug!("Drone {} removed sender {}", self.id, node_id);
                } else {
                    warn!(
                        "Drone {} could not remove sender {} because it is not a neighbour",
                        self.id, node_id
                    );
                }
            }
        }
    }

    pub fn handle_packet(&mut self, packet: Packet) {
        /// This function handles the received packet
        /// It checks the packet type and calls the appropriate function
        /// to handle the packet
        /// ### Parameters:
        /// - `packet`: The packet to be handled
        match packet.pack_type {
            PacketType::FloodRequest(flood_request) => {
                self.handle_flood_request(flood_request, packet.routing_header, packet.session_id)
            }

            PacketType::MsgFragment(_) => self.handle_fragment(packet),

            _ => self.handle_packet_forwarding(packet),
        }
    }

    /// This function handles the packet in case of a crash
    fn handle_packet_crash(&mut self, packet: Packet) {
        match packet.pack_type {
            PacketType::MsgFragment(_) => {
                self.generate_nack(
                    NackType::ErrorInRouting(self.id),
                    packet.routing_header.clone(),
                    packet.session_id,
                );
                if self
                    .controller_send
                    .send(DroneEvent::PacketDropped(packet))
                    .is_err()
                {
                    error!("Drone {} could not send packet to controller", self.id);
                }
            }

            PacketType::FloodRequest(_) => {
                self.generate_nack(
                    NackType::ErrorInRouting(self.id),
                    packet.routing_header.clone(),
                    packet.session_id,
                );
                let result = self.controller_send.send(DroneEvent::PacketDropped(packet));

                if result.is_err() {
                    error!("Drone {} could not send packet to controller", self.id);
                }
            }

            _ => self.handle_packet_forwarding(packet),
        }
    }

    fn handle_packet_forwarding(&self, mut packet: Packet) {
        /// This function handles the packet forwarding
        /// It checks if the packet is for this drone and
        /// if the next hop is in the list of neighbours
        /// if the next hop is not in the list of neighbours,
        /// it generates a nack of type NackType::ErrorInRouting
        /// else increases the hop index and forwards the packet to the next hop
        /// ### Parameters:
        /// - `packet`: The packet to be forwarded
        // Step 1: Check if the packet is for this drone
        if !self.is_correct_recipient(&packet.routing_header, packet.session_id) {
            return;
        }

        // Step 2: Check if there is a next node
        match &packet.routing_header.next_hop() {
            None => {
                // if there is no next hop, generate a nack with NackType::DestinationIsDrone
                self.generate_nack(
                    NackType::DestinationIsDrone,
                    packet.routing_header,
                    packet.session_id,
                );
                return;
            }
            Some(next_hop) => {
                // Step 3: increase the hop index
                packet.routing_header.increase_hop_index();
                self.forward_packet(packet, next_hop);
            }
        }
    }

    fn handle_fragment(&self, mut msg_packet: Packet) {
        /// This function handles the message fragment
        /// It checks if the packet is for this drone and
        /// if the next hop is in the list of neighbours
        /// than increases the hop index, checks if the packet should be dropped
        /// if the packet should be dropped, it generates a nack of type NackType::Dropped
        /// and sends it back to the source else it forwards the packet to the next hop
        /// ### Parameters:
        /// - `msg_packet`: The message packet
        // Step 1: Check if the packet is for this drone
        if !self.is_correct_recipient(&msg_packet.routing_header, msg_packet.session_id) {
            return;
        }

        // Step 3: Check if there is a next node
        match &msg_packet.routing_header.next_hop() {
            None => {
                // if there is no next hop, generate a nack with NackType::DestinationIsDrone
                self.generate_nack(
                    NackType::DestinationIsDrone,
                    msg_packet.routing_header,
                    msg_packet.session_id,
                );
                return;
            }
            Some(next_hop) => {
                // Step 2: Increase the hop index
                msg_packet.routing_header.increase_hop_index();

                // Step 4: Check if the next hop is in the list of neighbours
                if !self.packet_send.contains_key(&next_hop) {
                    self.generate_nack(
                        NackType::ErrorInRouting(*next_hop),
                        msg_packet.routing_header,
                        msg_packet.session_id,
                    );
                    return;
                }

                // Step 5: Check if the packet should be dropped
                let drop = rand::thread_rng().gen_range(0.0..1.0);
                if drop <= self.pdr {
                    if self
                        .controller_send
                        .send(DroneEvent::PacketDropped(msg_packet.clone()))
                        .is_err()
                    {
                        error!("Drone {} could not send packet to controller", self.id);
                    }
                    self.generate_nack(
                        NackType::Dropped,
                        msg_packet.routing_header,
                        msg_packet.session_id,
                    );
                    return;
                }

                self.forward_packet(msg_packet, next_hop);
            }
        }
    }

    fn handle_flood_request(
        &mut self,
        mut flood_request: FloodRequest,
        srh: SourceRoutingHeader,
        session_id: u64,
    ) {
        /// This function handles the flood request and forwards it to the neighbours
        /// If the drone has already seen the flood request, it generates a flood response
        /// or if the drone has no other neighbour other than
        /// the previous hop (the sender of the flood request), it generates a flood response
        /// ### Parameters:
        /// - `flood_request`: The flood request
        /// - `srh`: The source routing header of the packet
        /// - `session_id`: The session id of the packet
        let prev_hop = if flood_request.path_trace.last().is_some() {
            flood_request.path_trace.last().unwrap().0
        } else {
            error!(
                "Drone {} received flood request with empty path trace",
                self.id
            );
            return;
        };

        flood_request.path_trace.push((self.id, NodeType::Drone));

        let flood_session = (flood_request.flood_id, flood_request.initiator_id);
        if self.flood_session.contains(&flood_session) {
            self.generate_flood_response(flood_request, session_id);
            return;
        }
        self.flood_session.insert(flood_session);

        if self.packet_send.len() == 1 && self.packet_send.contains_key(&prev_hop) {
            self.generate_flood_response(flood_request, session_id);
            return;
        }

        let new_flood_request = Packet::new_flood_request(srh, session_id, flood_request);

        for neighbor in &self.packet_send {
            if *neighbor.0 != prev_hop {
                let result = match neighbor.1.send(new_flood_request.clone()) {
                    Ok(_) => self
                        .controller_send
                        .send(DroneEvent::PacketSent(new_flood_request.clone())),
                    Err(_) => self
                        .controller_send
                        .send(DroneEvent::ControllerShortcut(new_flood_request.clone())),
                };

                if result.is_err() {
                    error!("Drone {} could not send packet to controller", self.id);
                }
            }
        }
    }

    fn generate_nack(&self, nack_type: NackType, mut srh: SourceRoutingHeader, session_id: u64) {
        /// This function generates a nack of type: nack_type, and sends it to the next hop
        /// ### Parameters:
        /// - `nack_type`: The type of nack to be generated
        /// - `srh`: The source routing header of the packet
        /// - `session_id`: The session id of the packet
        let nack = Nack {
            fragment_index: 0,
            nack_type,
        };

        // if the route is malformed, send a nack to the controller
        if srh.len() == 1 {
            let new_nack = Packet::new_nack(srh, session_id, nack);
            if self
                .controller_send
                .send(DroneEvent::ControllerShortcut(new_nack))
                .is_err()
            {
                error!("Drone {} could not send packet to controller", self.id);
            }
            return;
        }

        match nack_type {
            NackType::ErrorInRouting(_) | NackType::Dropped => {
                // reverse the trace
                srh = srh.sub_route(0..srh.hop_index).unwrap();
                srh.hops.reverse();
                srh.hop_index = 1;
            }
            NackType::UnexpectedRecipient(_) => {
                // reverse the trace
                srh = srh.sub_route(0..=srh.hop_index).unwrap();
                srh.hops.pop();
                srh.hops.push(self.id);
                srh.hops.reverse();
                srh.hop_index = 1;
            }
            _ => {
                // reverse the trace
                srh = srh.sub_route(0..=srh.hop_index).unwrap();
                srh.hops.reverse();
                srh.hop_index = 1;
            }
        }

        let new_nack = Packet::new_nack(srh, session_id, nack);

        // get the next hop (use current_hop() instead of next_hop() because
        // the hop index is reset to 1)
        let next_hop = &new_nack.routing_header.current_hop().unwrap();

        // send nack to next hop, if send fails, send to controller
        self.forward_packet(new_nack, next_hop);
    }

    fn generate_flood_response(&self, flood_request: FloodRequest, session_id: u64) {
        /// This function generates a flood response and sends it to the next hop
        /// ### Parameters:
        /// - `flood_request`: The flood request
        /// - `session_id`: The session id of the packet
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
            }
            Some(next_hop) => {
                srh.increase_hop_index();
                let new_flood_response =
                    Packet::new_flood_response(srh, session_id, flood_response);
                self.forward_packet(new_flood_response, next_hop);
            }
        }
    }

    fn is_correct_recipient(&self, srh: &SourceRoutingHeader, session_id: u64) -> bool {
        /// This function checks if the packet is for this drone
        /// If the packet is not for this drone, a nack of type
        /// NackType::UnexpectedRecipient is generated and sent back to the source
        /// ### Parameters:
        /// - `srh`: The source routing header of the packet
        /// - `session_id`: The session id of the packet
        ///
        /// ### Returns:
        /// - `bool`: True if the packet is for this drone, false otherwise
        let current_hop = srh.current_hop();
        if current_hop.is_some() && current_hop != Some(self.id) {
            self.generate_nack(
                NackType::UnexpectedRecipient(current_hop.unwrap()),
                srh.clone(),
                session_id,
            );
            false
        } else {
            if current_hop.is_none() {
                error!("Drone {} received packet with empty route", self.id);
                return false;
            }
            true
        }
    }

    fn forward_packet(&self, packet: Packet, next_hop: &NodeId) {
        /// This function is responsible for forwarding the packet to the next hop
        /// If the next hop is not in the list of neighbours, a nack is generated
        /// and sent back to the source
        /// ### Parameters:
        /// - `packet`: The packet to be forwarded
        /// - `next_hop`: The next hop id to which the packet should be forwarded
        // step 4: Identify the sender and check if is valid
        match self.packet_send.get(next_hop) {
            Some(sender) => {
                // step 5: send the packet to the next hop
                // if the send() is successful, send an event to the controller
                if sender.send(packet.clone()).is_ok() {
                    // if the send fails, log an error
                    if self
                        .controller_send
                        .send(DroneEvent::PacketSent(packet))
                        .is_err()
                    {
                        error!("Drone {} could not send packet to controller", self.id);
                    }
                } else {
                    let result = {
                        if let PacketType::MsgFragment(_) = packet.pack_type {
                            self.controller_send.send(DroneEvent::PacketDropped(packet))
                        } else {
                            self.controller_send
                                .send(DroneEvent::ControllerShortcut(packet))
                        }
                    };

                    if result.is_err() {
                        error!("Drone {} could not send packet to controller", self.id);
                    }
                }
            }
            None => {
                // step 4.1: if the next hop is not in the list of neighbours, generate a nack with
                // NackType::ErrorInRouting and send it back to the source
                self.generate_nack(
                    NackType::ErrorInRouting(*next_hop),
                    packet.routing_header,
                    packet.session_id,
                );
            }
        }
    }
}
