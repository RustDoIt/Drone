extern crate wg_2024;

use wg_2024::{drone::DroneOptions};
use wg_2024::drone::{self, Drone};
use wg_2024::controller::{DroneCommand, NodeEvent};
use wg_2024::controller;
use wg_2024::network::NodeId;
use wg_2024::packet::{Packet, PacketType};
use wg_2024::config::{self, Config};
use crossbeam_channel::{bounded, select_biased, unbounded};
use crossbeam_channel::{Receiver, Sender};

use std::collections::HashMap;
use std::{fs, thread};

struct RustDoIt {
    id: NodeId,
    controller_send: Sender<NodeEvent>,
    controller_recv: Receiver<DroneCommand>,
    packet_recv: Receiver<Packet>,
    pdr: f32,
    packet_send: HashMap<NodeId, Sender<Packet>>,
}

impl drone::Drone for RustDoIt {

    fn new(options: DroneOptions) -> Self {
        return Self {
            id: options.id,
            controller_send: options.controller_send,
            controller_recv: options.controller_recv,
            packet_recv: options.packet_recv,
            pdr: options.pdr,
            packet_send: HashMap::new(),
        };
    }

    fn run(&mut self) {
        loop {
            select_biased! {
                recv(self.controller_recv) -> command => {
                    if let Ok(command) = command {
                        self.handle_command(command);
                    }
                },

                recv(self.packet_recv) -> packet => {
                    if let Ok(packet) = packet {
                        self.handle_packet(packet);
                    }
                }
            }
        }
    }
}

impl RustDoIt {

    fn handle_packet(&mut self, packet: Packet) {
        match packet.pack_type {
            PacketType::Nack(_nack) => todo!(),
            PacketType::Ack(_ack) => todo!(),
            PacketType::MsgFragment(_fragment) => todo!(),
            PacketType::FloodRequest(_flood_request) => todo!(),
            PacketType::FloodResponse(_flood_response) => todo!(),
        }
    }

    fn handle_command(&mut self, command: DroneCommand) {
        match command {
            DroneCommand::AddSender(node_id, sender) => {

            },

            DroneCommand::SetPacketDropRate(pdr) => {
                
                if self.pdr < 0.0 || self.pdr > 1.0 {
                    return;
                }

                self.pdr = pdr;
            },

            DroneCommand::Crash => unreachable!(),
        }
    }
}

struct SimulationController {
    drones: HashMap<NodeId, Sender<DroneCommand>>,
    node_event_recv: Receiver<NodeEvent>
}

// impl controller::SimulationController for SimulationController {

// }

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
}

fn parse_config(file: &str) -> Config {
    let file_str = fs::read_to_string(file).unwrap();
    toml::from_str(&file_str).unwrap()
}

fn main() {

    let config = parse_config("src/config.toml");

    //? PACCHETTI

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

    //? SIMULATION CONTROLLER

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

        handles.push(thread::spawn(move || {

            // Creo il drone
            let mut drone = RustDoIt::new(DroneOptions {
                id: drone.id,
                controller_recv: controller_drone_recv, // Do al drone il canale per ricevere i comandi del controller
                controller_send: node_event_send, // Uso il canale send per inviare al controller i log
                packet_recv: packet_recv, // Do al drone il receiver per ricevere i pacchetti
                packet_send: packet_send, // Do al drone una hashmap per comunicare a tutti i droni COLLEGATI 
                pdr: drone.pdr
            });
            
            // Eseguo il drone
            drone.run();
        }));
    }

    // Creao il controller
    let mut controller = SimulationController {
        drones: drones,
        node_event_recv: node_event_recv
    };

    // Controller
    loop {
        select_biased! {
            recv(controller.node_event_recv) -> event => {
                break;
            },
        }
    }

    controller.crash_all();

    // Aspetta fino a quando tutti i thread non terminano
    while let Some(handle) = handles.pop() {
        handle.join().unwrap();
    }

}
