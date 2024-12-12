use std::collections::{HashMap, HashSet};
use log::{info, warn, error, debug};
use rand::Rng;
use wg_2024::drone::{Drone};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::NodeId;
use wg_2024::packet::{Nack, NackType, Packet, PacketType, NodeType, FloodRequest, FloodResponse};
use wg_2024::network::SourceRoutingHeader;
use crossbeam_channel::select_biased;
use crossbeam_channel::{Receiver, Sender};

pub mod rust_do_it;
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