#![forbid(unsafe_code)]

use std::error::Error;
use std::time::Duration;

use rumqttc::{self, Client, MqttOptions, QoS};

mod clean_retained;
mod cli;
mod format;
mod interactive;
mod json_view;
mod log;
mod mqtt_packet;
mod publish;
mod topic;

fn main() -> Result<(), Box<dyn Error>> {
    let matches = cli::build().get_matches();

    let host = matches.get_one::<String>("Broker").unwrap();
    let port = *matches.get_one::<u16>("Port").unwrap();

    let client_id = format!("mqttui-{:x}", rand::random::<u32>());
    let mut mqttoptions = MqttOptions::new(client_id, host, port);
    mqttoptions.set_max_packet_size(usize::MAX, usize::MAX);

    if let Some(password) = matches.get_one::<String>("Password") {
        let username = matches.get_one::<String>("Username").unwrap();
        mqttoptions.set_credentials(username, password);
    }

    if let Some(matches) = matches.subcommand_matches("clean-retained") {
        let timeout = Duration::from_secs_f32(*matches.get_one("Timeout").unwrap());
        mqttoptions.set_keep_alive(timeout);
    }

    let (mut client, connection) = Client::new(mqttoptions, 10);

    match matches.subcommand() {
        Some(("clean-retained", matches)) => {
            let topic = matches.get_one::<String>("Topic").unwrap();
            let mode = if matches.contains_id("dry-run") {
                clean_retained::Mode::Dry
            } else {
                clean_retained::Mode::Normal
            };
            client.subscribe(topic, QoS::AtLeastOnce)?;
            clean_retained::clean_retained(client, connection, mode);
        }
        Some(("log", matches)) => {
            let verbose = matches.contains_id("verbose");
            for topic in matches.get_many::<String>("Topics").unwrap() {
                client.subscribe(topic, QoS::AtLeastOnce)?;
            }
            log::show(connection, verbose);
        }
        Some(("publish", matches)) => {
            let verbose = matches.contains_id("verbose");
            let retain = matches.contains_id("retain");
            let topic = matches.get_one::<String>("Topic").unwrap();
            let payload = matches.get_one::<String>("Payload").unwrap().as_str();
            client.publish(topic, QoS::AtLeastOnce, retain, payload)?;
            publish::eventloop(client, connection, verbose);
        }
        Some((command, _)) => unreachable!("command is not available: {}", command),
        None => {
            let topic = matches.get_one::<String>("Topic").unwrap();
            interactive::show(client.clone(), connection, host, port, topic)?;
            client.disconnect()?;
        }
    }

    Ok(())
}
