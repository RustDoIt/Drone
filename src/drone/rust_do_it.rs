extern crate wg_2024;

use crossbeam_channel::Sender;
use log::{warn, error, debug, info};
use rand::Rng;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Nack, NackType, Packet, PacketType, NodeType, FloodRequest, FloodResponse};
use super::RustDoIt;


impl RustDoIt {

    pub fn handle_command(&mut self, command: DroneCommand) {
        // This function handles the command received from the controller
        // It checks the command type and takes the appropriate actions
        // to handle the command
        // ### Parameters:
        // - `command`: The command to be handled

        match command {
            DroneCommand::AddSender(node_id, sender) => {
                self.packet_send.insert(node_id, sender);
                debug!("Drone {} added sender {}", self.id, node_id);
            },

            DroneCommand::SetPacketDropRate(pdr) => {
                if (0.0..=1.0).contains(&pdr) {
                    self.pdr = pdr;
                    debug!("Drone {} set PDR to {}", self.id, self.pdr);
                } else {
                    warn!(
                        "Drone {} could not set PDR to {}, value must be between 0.0 and 1.0",
                        self.id,
                        pdr
                    );
                }
            },

            DroneCommand::Crash => {
                while let Ok(packet) = self.packet_recv.try_recv() {
                    self.handle_packet_crash(packet);
                    debug!("Drone {} crashed", self.id);
                };
            },

            DroneCommand::RemoveSender(node_id) => {
                if let Some(_removed_sender) = self.packet_send.remove(&node_id) {
                    debug!("Drone {} removed sender {}", self.id, node_id);
                } else {
                    warn!(
                        "Drone {} could not remove sender {} because it is not a neighbour",
                        self.id,
                        node_id
                    );
                }
            },
        }
    }


    pub fn handle_packet(&mut self, packet: Packet) {
        // This function handles the received packet
        // It checks the packet type and calls the appropriate function
        // to handle the packet
        // ### Parameters:
        // - `packet`: The packet to be handled

        match packet.pack_type {
            PacketType::FloodRequest(flood_request) => self.handle_flood_request(
                flood_request,
                packet.session_id
            ),

            _ => self.check_packet(packet)

        }
    }

    /// This function handles the packet in case of a crash
    fn handle_packet_crash(&mut self, packet: Packet) {
        match packet.pack_type {
            PacketType::MsgFragment(_) => {
                self.generate_nack(
                    NackType::ErrorInRouting(self.id),
                    packet.get_fragment_index(),
                    packet.routing_header.clone(),
                    packet.session_id
                );
                if self.controller_send.send(DroneEvent::PacketDropped(packet)).is_err() {
                    error!("Drone {} could not send packet to controller", self.id);
                }
            },

            PacketType::FloodRequest(_) => {
                self.generate_nack(
                    NackType::ErrorInRouting(self.id),
                    packet.get_fragment_index(),
                    packet.routing_header.clone(),
                    packet.session_id
                );
                let result = self.controller_send
                    .send(DroneEvent::PacketDropped(packet));

                if result.is_err() {
                    error!("Drone {} could not send packet to controller", self.id);
                }
            },

            _ => self.check_packet(packet)

        }
    }

    fn check_packet(&self, mut packet: Packet) {
        // This function is responsible for checking the packet before forwarding it.
        // It checks if the packet is for this drone and
        // if the next hop is in the list of neighbours
        // if the next hop is not in the list of neighbours,
        // it generates a nack of type NackType::ErrorInRouting
        // else increases the hop index and forwards the packet to the next hop
        // ### Parameters:
        // - `packet`: The packet to be forwarded

        // Step 0: check if the packet is droppable or not
        let droppable = matches!(packet.pack_type, PacketType::MsgFragment(_));

        // Step 1: Check if the packet is for this drone
        if !self.is_correct_recipient(
            packet.get_fragment_index(),
            &packet.routing_header,
            packet.session_id
        ) {
            return;
        }

        // Step 2: Check if there is a next node
        match packet.routing_header.next_hop() {
            None => {
                self.generate_nack(
                    NackType::DestinationIsDrone,
                    packet.get_fragment_index(),
                    packet.routing_header,
                    packet.session_id
                );

            },
            Some(next_hop) => {
                // Step 3: increase the hop index
                packet.routing_header.increase_hop_index();

                // Step 4: Check if the next hop is in the list of neighbours
                let next_hop = match self.packet_send.get(&next_hop) {
                    Some(sender) => sender,
                    None => {
                        // step 4.1: if the next hop is not in the list of neighbours, generate a nack with
                        // NackType::ErrorInRouting and send it back to the source
                        warn!("Drone {} not found in the list of neighbours, probably crashed.", &next_hop);
                        match packet.pack_type {
                            PacketType::Ack(_) | PacketType::Nack(_) | PacketType::FloodResponse(_) => {
                                info!("Packet sent to controller");
                                self.controller_send.send(DroneEvent::ControllerShortcut(packet.clone())).ok();
                                return;
                            },
                            _ => {
                                self.generate_nack(
                                    NackType::ErrorInRouting(next_hop),
                                    packet.get_fragment_index(),
                                    packet.routing_header.clone(),
                                    packet.session_id
                                );
                                return;
                            }
                        }
                    }
                };

                // check if the packet should be dropped
                if droppable && self.is_dropped() {
                    let mut dropped_message = packet.clone();
                    dropped_message.routing_header.decrease_hop_index();
                    if self.controller_send
                        .send(DroneEvent::PacketDropped(dropped_message))
                        .is_err() {
                        error!("Drone {} could not send packet to controller", self.id);
                    }

                    self.generate_nack(
                        NackType::Dropped,
                        packet.get_fragment_index(),
                        packet.routing_header,
                        packet.session_id,
                    );
                    return;
                }

                self.forward_packet(packet, &next_hop);
            }
        }
    }


    fn handle_flood_request(
        &mut self,
        mut flood_request: FloodRequest,
        session_id: u64
    ) {
        // This function handles the flood request and forwards it to the neighbours
        // If the drone has already seen the flood request, it generates a flood response
        // or if the drone has no other neighbour other than
        // the previous hop (the sender of the flood request), it generates a flood response
        // ### Parameters:
        // - `flood_request`: The flood request
        // - `srh`: The source routing header of the packet
        // - `session_id`: The session id of the packet

        let prev_hop = flood_request.path_trace
            .last()
            .map(|x| x.0)
            .unwrap_or(flood_request.initiator_id);

        flood_request.path_trace.push((self.id, NodeType::Drone));


        let flood_session = (flood_request.flood_id, flood_request.initiator_id);

        if !self.flood_session.insert(flood_session) || self.packet_send.len() == 1 {
            self.generate_flood_response(flood_request, session_id);
            return;
        }

        let srh = SourceRoutingHeader::new(vec![], 0);

        let new_flood_request = Packet::new_flood_request(
            srh,
            session_id,
            flood_request,
        );

        for (neighbor_id, neighbor) in self.packet_send.iter() {
            if *neighbor_id != prev_hop {
                let result = match neighbor.send(new_flood_request.clone()) {
                    Ok(_) => self.controller_send.send(DroneEvent::PacketSent(new_flood_request.clone())),
                    Err(_) => self.controller_send.send(DroneEvent::ControllerShortcut(new_flood_request.clone())),
                };

                if result.is_err() {
                    error!("Drone {} could not send packet to controller", self.id);
                }
            }
        }
    }

    fn generate_nack(
        &self,
        nack_type: NackType,
        fragment_index: u64,
        mut srh: SourceRoutingHeader,
        session_id: u64
    ) {
        // This function generates a nack of type: nack_type, and sends it to the next hop
        // ### Parameters:
        // - `nack_type`: The type of nack to be generated
        // - `srh`: The source routing header of the packet
        // - `session_id`: The session id of the packet

        let nack = Nack {
            fragment_index,
            nack_type,
        };

        // if the route is malformed, send a nack to the controller
        if srh.len() == 1 {
            let new_nack = Packet::new_nack(
                srh,
                session_id,
                nack,
            );
            if self.controller_send.send(DroneEvent::ControllerShortcut(new_nack)).is_err() {
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
            },
            NackType::UnexpectedRecipient(_) => {
                // reverse the trace
                srh = srh.sub_route(0..=srh.hop_index).unwrap();
                srh.hops.pop();
                srh.hops.push(self.id);
                srh.hops.reverse();
                srh.hop_index = 1;
            },
            NackType::DestinationIsDrone => {
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

        // get the next hop (use current_hop() instead of next_hop() because
        // the hop index is reset to 1)
        let next_hop = new_nack.routing_header.current_hop().unwrap();
        let next_hop = match self.packet_send.get(&next_hop) {
            None => {
                self.controller_send.send(DroneEvent::ControllerShortcut(new_nack)).ok();
                return;
            }
            Some(hop) => hop
        };

        // send nack to next hop, if send fails, send to controller
        self.forward_packet(new_nack, &next_hop);

    }

    fn generate_flood_response(&self, flood_request: FloodRequest, session_id: u64) {
        // This function generates a flood response and sends it to the next hop
        // ### Parameters:
        // - `flood_request`: The flood request
        // - `session_id`: The session id of the packet

        let mut route: Vec<_> = flood_request.path_trace
            .clone()
            .iter()
            .map(|(id, _)| *id)
            .rev()
            .collect::<Vec<_>>();


        if route.last() != Some(&flood_request.initiator_id){
            route.push(flood_request.initiator_id);
        }

        let flood_response = FloodResponse {
            flood_id: flood_request.flood_id,
            path_trace: flood_request.path_trace,
        };

        let mut srh = SourceRoutingHeader::new(route, 0);
        match &srh.next_hop() {
            None => {
                self.generate_nack(
                    NackType::DestinationIsDrone,
                    0,
                    srh,
                    session_id
                );
            },
            Some(next_hop) => {
                srh.increase_hop_index();
                let new_flood_response = Packet::new_flood_response(
                    srh,
                    session_id,
                    flood_response
                );

                match self.packet_send.get(&next_hop) {
                    None => {
                        self.controller_send.send(DroneEvent::ControllerShortcut(new_flood_response)).ok();
                        return;
                    }
                    Some(hop) => self.forward_packet(new_flood_response, hop)
                }
            }
        }
    }

    fn is_correct_recipient(
        &self,
        fragment_index: u64,
        srh: &SourceRoutingHeader,
        session_id: u64
    ) -> bool {
        // This function checks if the packet is for this drone
        // If the packet is not for this drone, a nack of type
        // NackType::UnexpectedRecipient is generated and sent back to the source
        // ### Parameters:
        // - `srh`: The source routing header of the packet
        // - `session_id`: The session id of the packet
        //
        // ### Returns:
        // - `bool`: True if the packet is for this drone, false otherwise

        let current_hop = srh.current_hop();
        if current_hop != Some(self.id) {
            self.generate_nack(
                NackType::UnexpectedRecipient(self.id),
                fragment_index,
                srh.clone(),
                session_id
            );
            false
        } else {
            true
        }
    }

    fn is_dropped(&self) -> bool {
        let drop = rand::thread_rng().gen_range(0.0..1.0);
        drop <= self.pdr
    }

    fn forward_packet(
        &self,
        packet: Packet,
        next_hop: &Sender<Packet>,
    ) {
        // This function is responsible for forwarding the packet to the next hop
        // If the next hop is not in the list of neighbours, a nack is generated
        // and sent back to the source
        // ### Parameters:
        // - `packet`: The packet to be forwarded
        // - `next_hop`: The next hop id to which the packet should be forwarded

        if next_hop.send(packet.clone()).is_ok() {
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
    }
}
