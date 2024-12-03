extern crate wg_2024;

use rand::Rng;
use wg_2024::drone::{self, Drone};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::controller;
use wg_2024::network::NodeId;
use wg_2024::packet::{Ack, Nack, NackType, Packet, PacketType};
use wg_2024::config::{self, Config};
use wg_2024::network::{self, SourceRoutingHeader};
use crossbeam_channel::{bounded, select_biased, unbounded};
use crossbeam_channel::{Receiver, Sender};
use wg_2024::packet::Fragment;
use std::time::Duration;

use std::collections::HashMap;
use std::{fs, thread};

#[derive(Debug)]
struct RustDoIt {
    id: NodeId,
    controller_send: Sender<DroneEvent>,                 // Used to send events to the controller (receiver is in the controller)
    controller_recv: Receiver<DroneCommand>,            // Used to receive commands from the controller (sender is in the controller)
    packet_recv: Receiver<Packet>,                      // The receiving end of the channel for receiving packets from drones
    packet_send: HashMap<NodeId, Sender<Packet>>,       // Mapping of drone IDs to senders, allowing packets to be sent to specific drones

    pdr: f32,
}

impl drone::Drone for RustDoIt {

    fn new(
        id: NodeId,
        controller_send: Sender<DroneEvent>,
        controller_recv: Receiver<DroneCommand>,
        packet_recv: Receiver<Packet>,
        packet_send: HashMap<NodeId, Sender<Packet>>,
        pdr: f32,
    ) -> Self {
        return Self {
            id: id,
            controller_send: controller_send,
            controller_recv: controller_recv,
            packet_recv: packet_recv,
            packet_send: packet_send,
            pdr: pdr,
        };
    }

    fn run(&mut self) {
        println!("Drone {} is Running",self.id);

        loop {
            select_biased! {
                recv(self.controller_recv) -> command => {

                    match command {
                        Ok(drone_command) => {},
                        Err(err) => println!("Error: {:?}", err)
                    }

                    // println!("Qualcosa controller_recv");
                    // if let Ok(command) = command {
                    //     if self.handle_command(command) {
                    //         break;
                    //     }
                    // }
                },

                recv(self.packet_recv) -> packet => {
                    println!("Qualcosa packet_recv");
                    if let Ok(packet) = packet {
                        println!("Ok packet_recv");
                        self.handle_packet(packet);
                    }
                    else {
                        println!("Error packet_recv");
                    }
                }
            }
        }
    }
}

impl RustDoIt {

    fn handle_packet(&mut self, packet: Packet) {
        println!("Received packet of type {:?}",packet.pack_type);
        match packet.pack_type {
            //when i receive a NACK, the routing header is reversed, so instead of -1 i perfom a +1 to go to the "previous" node
            PacketType::Nack(_nack) => {
                
                let next_node = packet.routing_header.hops[packet.routing_header.hop_index + 1];

                if let Some(sender) = self.packet_send.get(&next_node) {
                    
                    let mut routing_header = packet.routing_header.clone();
                    routing_header.hop_index += 1;
                    
                    let nack = Packet {
                        pack_type: PacketType::Nack(_nack),
                        routing_header: routing_header.clone(),
                        session_id: packet.session_id
                    };

                    sender.send(nack).unwrap();
                }
                else {
                    println!("ERROREEEEEE")        
                }
            },

            PacketType::Ack(_ack) => {

                let next_node = packet.routing_header.hops[packet.routing_header.hop_index + 1];

                if let Some(sender) = self.packet_send.get(&next_node) {

                    let mut routing_header = packet.routing_header.clone();
                    routing_header.hop_index += 1;

                    let ack = Packet {
                        pack_type: PacketType::Ack(Ack {
                            fragment_index: _ack.fragment_index
                        }),
                        routing_header: routing_header.clone(),
                        session_id: packet.session_id
                    };

                    sender.send(ack).unwrap();
                }
                else {
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
                        // This drone is the final destination  -> Error (the destination cannot be a drone)
                        
                        let mut routing_header = packet.routing_header.clone();
                        
                        routing_header.hops.reverse(); //reverse the trace
                        routing_header.hop_index = routing_header.hops.len() - routing_header.hop_index - 1;//starting node

                        routing_header.hop_index += 1;//this refer to the next node (in the reversed array)

                        let nack = Packet {
                            pack_type: PacketType::Nack(Nack {fragment_index: fragment.fragment_index, nack_type: NackType::DestinationIsDrone}),
                            routing_header: routing_header.clone(),
                            session_id: packet.session_id
                        };
    
                        //send
                        let sender = self.packet_send.get(&packet.routing_header.hops[packet.routing_header.hop_index]).unwrap();
                        sender.send(nack).unwrap();

                        return;
                    }
                    else {
                        // if i'm not the last node, i try to see if the next node is reachable (is one of my neighbour)
                        let mut routing_header = packet.routing_header.clone();

                        //take the next node and prepare the packet for the next node
                        routing_header.hop_index += 1;
                        let new_packet = Packet {
                            pack_type: PacketType::MsgFragment(fragment.clone()),
                            routing_header: routing_header.clone(), 
                            session_id: packet.session_id
                        };
                        let next_hop = routing_header.hops[new_packet.routing_header.hop_index];
                        //if the next node is reachable
                        if let Some(next_node) = self.packet_send.get(&next_hop) {
                            
                            let drop = rand::thread_rng().gen_range(0.0..1.0);
                            //STEP 5
                            if drop <= self.pdr {
                                //the packet need to be dropped, and a nack must be sent
                                let mut routing_header = packet.routing_header.clone();
                        
                                routing_header.hops.reverse(); //reverse the trace
                                routing_header.hop_index = routing_header.hops.len() - routing_header.hop_index - 1;//starting node

                                routing_header.hop_index += 1;//this refer to the next node (in the reversed array)

                                let nack = Packet {
                                    pack_type: PacketType::Nack(Nack { fragment_index: fragment.fragment_index, nack_type: NackType::Dropped }),
                                    routing_header: routing_header.clone(), //
                                    session_id: packet.session_id
                                };
                                
                                //send
                                let sender = self.packet_send.get(&packet.routing_header.hops[packet.routing_header.hop_index]).unwrap();
                                sender.send(nack).unwrap();

                                return;
                            }
                            else {
                                
                                next_node.send(new_packet).unwrap();
                                return;
                            }

                        }
                        else {
                            // STEP 4 : if it is not in my neighbour, send a nack of type error in routing
                            let mut routing_header = packet.routing_header.clone();
                        
                            routing_header.hops.reverse(); //reverse the trace
                            routing_header.hop_index = routing_header.hops.len() - routing_header.hop_index - 1;//starting node

                            routing_header.hop_index += 1;//this refer to the next node (in the reversed array)
                                
                            let nack = Packet {
                                pack_type: PacketType::Nack(Nack { fragment_index: fragment.fragment_index, nack_type: NackType::ErrorInRouting(next_hop) }),
                                routing_header: routing_header.clone(), //
                                session_id: packet.session_id
                            };
                            
                            //send
                            let sender = self.packet_send.get(&packet.routing_header.hops[packet.routing_header.hop_index]).unwrap();
                            sender.send(nack).unwrap();

                            return;
                        }
                    }

                }
                else {

                    let mut routing_header = packet.routing_header.clone();
                        
                    routing_header.hops.reverse(); //reverse the trace
                    routing_header.hop_index = routing_header.hops.len() - routing_header.hop_index - 1;//starting node

                    routing_header.hop_index += 1;//this refer to the next node (in the reversed array)

                    let nack = Packet {
                        pack_type: PacketType::Nack(Nack { fragment_index: fragment.fragment_index, nack_type: NackType::UnexpectedRecipient(self.id) }),
                        routing_header: packet.routing_header.clone(),
                        session_id: packet.session_id
                    };

                    //send
                    let sender = self.packet_send.get(&packet.routing_header.hops[packet.routing_header.hop_index]).unwrap();
                    sender.send(nack).unwrap();

                    return;
                }
                
            },

            PacketType::FloodRequest(_flood_request) => {
                todo!()
            },

            PacketType::FloodResponse(_flood_response) => {
                todo!()
            },
        }
    }

    fn handle_command(&mut self, command: DroneCommand) -> bool {
        match command {
            DroneCommand::AddSender(node_id, sender) => {
                
                if self.packet_send.contains_key(&node_id) {
                    return false;
                }

                self.packet_send.insert(node_id, sender);
                false
            },

            DroneCommand::SetPacketDropRate(pdr) => {
                
                if self.pdr > 0.0 || self.pdr < 1.0 {
                    return false;
                }

                self.pdr = pdr;
                false
            },

            DroneCommand::Crash => true,
            //DroneCommand::RemoveSender(_) => {unimplemented!()}
            DroneCommand::RemoveSender(node_id) => {
                if self.packet_send.remove(&node_id).is_some() {
                    true // Ritorna true se il mittente è stato rimosso con successo
                } else {
                    false // Ritorna false se non c'era un mittente associato a node_id
                }
            },
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
}

fn parse_config(file: &str) -> Config {
    let file_str = fs::read_to_string(file).unwrap();
    toml::from_str(&file_str).unwrap()
}

fn main() {

    // let config = parse_config("src/config.toml");
    //
    // //? PACCHETTI
    //
    // // Da ad ogni drone un canale per scambiare pacchetti (unbounded)
    // let mut packet_channels = HashMap::new();
    // for drone in config.drone.iter() {
    //     packet_channels.insert(drone.id, unbounded());
    // }
    //
    // // Da ad ogni client un canale per scambiare pacchetti (unbounded)
    // for client in config.client.iter() {
    //     packet_channels.insert(client.id, unbounded());
    // }
    //
    // // Da ad ogni server un canale per scambiare pacchetti (unbounded)
    // for server in config.server.iter() {
    //     packet_channels.insert(server.id, unbounded());
    // }
    //
    // //? SIMULATION CONTROLLER
    //
    // let mut handles = Vec::new();
    //
    // let mut drones = HashMap::new();
    // let (node_event_send, node_event_recv) = unbounded();
    //
    // for drone in config.drone.into_iter() {
    //
    //     // Da al controller un canale per comunicare con ogni drone
    //     let (controller_drone_send, controller_drone_recv) = unbounded();
    //     drones.insert(drone.id, controller_drone_send);
    //
    //     // Canale drone -> controller per i log
    //     let node_event_send = node_event_send.clone();
    //
    //     // Clona il receiver di ogni drone
    //     let packet_recv = packet_channels[&drone.id].1.clone();
    //
    //     // Prende tutti i sender verso ogni drone
    //     let packet_send: HashMap<NodeId, Sender<Packet>> = drone
    //         .connected_node_ids
    //         .into_iter()
    //         .map(|id| (id, packet_channels[&id].0.clone()))
    //         .collect();
    //
    //     handles.push(thread::spawn(move || {
    //
    //         // Creo il drone
    //         let mut drone = RustDoIt::new(
    //             drone.id,
    //             node_event_send, // Uso il canale send per inviare al controller i log
    //             controller_drone_recv, // Do al drone il canale per ricevere i comandi del controller
    //             packet_recv, // Do al drone il receiver per ricevere i pacchetti
    //             packet_send, // Do al drone una hashmap per comunicare a tutti i droni COLLEGATI
    //             drone.pdr
    //         );
    //
    //         // Eseguo il drone
    //         drone.run();
    //     }));
    // }
    //
    // // Crea il controller
    // let mut controller = SimulationController {
    //     drones: drones,
    //     node_event_recv: node_event_recv
    // };
    //
    // // Controller
    // // loop {
    // //     select_biased! {
    // //         recv(controller.node_event_recv) -> event => {
    // //             break;
    // //         },
    // //     }
    // // }
    //
    // controller.crash_all();
    //
    // // Aspetta fino a quando tutti i thread non terminano
    // while let Some(handle) = handles.pop() {
    //     handle.join().unwrap();
    // }
    //
    // println!("Stop")

    let (drone_send, drone_recv) = unbounded();
    let (controller_send, controller_recv) = unbounded();
    //while let Ok(_) = controller_recv.try_recv() {}

    let (packet_send, packet_recv) = unbounded();
    //while let Ok(_) = packet_recv.try_recv() {}

    let mut controller = SimulationController {
        drones: HashMap::from([(1, controller_send)]),
        node_event_recv: drone_recv
    };

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
        routing_header: routing_header,
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

    #[test]
    #[cfg(feature = "partial_eq")]
    pub fn generic_packet_forward(){
        let (d_send, d_recv) = unbounded();
        let (d2_send, d2_recv) = unbounded::<Packet>();
        let (_d_command_send, d_command_recv) = unbounded();
        let neighbours = HashMap::from([(12, d2_send.clone())]);

        let mut drone11 = RustDoIt::new(
            11,
            unbounded().0,
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
        msg.routing_header.hop_index = 2;

        // d2 receives packet from d1

        let packet_received = d2_recv.recv().unwrap();
        println!("{:?}", packet_received);
        assert_eq!(packet_received.routing_header.hop_index, 2);
        assert_eq!(packet_received, msg);
    }

    #[test]
    //#[cfg(feature = "partial_eq")]
    pub fn generic_nack_forward() {
        let (d1_send, d1_recv) = unbounded();
        let (d2_send, d2_recv) = unbounded::<Packet>();
        let (_d_command_send, d_command_recv) = unbounded();
        let neighbours = HashMap::from([(1, d2_send.clone())]);

        let mut drone11 = RustDoIt::new(
            11,
            unbounded().0,
            d_command_recv,
            d1_recv.clone(),
            neighbours,
            0.0,
        );
        thread::spawn(move || {
            drone11.run();
        });

        let mut msg = create_sample_packet();

        // "Client" sends packet to d
        d1_send.send(msg.clone()).unwrap();
        msg.routing_header.hop_index = 2;

        // d2 receives packet from d1

        let packet_received = d2_recv.recv().unwrap();
        println!("RECEIVED: {:?}", packet_received);
        println!("TYPE: {:?}", packet_received.pack_type);
        //PacketType::Nack(Nack {fragment_index: fragment.fragment_index, nack_type: NackType::DestinationIsDrone})

        //assert!(matches!(packet_received.pack_type, PacketType::Nack(_)));
    }
}
