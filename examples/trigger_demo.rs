use embedded_io_adapters::tokio_1::FromTokio;
use log::{error, info};
use rust_mqtt::{
    client::{client::Client, options::ConnectOptions, options::PublicationOptions},
    packet::v5::reason_codes::ReasonCode,
    types::{Bytes, QoS},
};
use tokio::net::TcpStream;

/// This demo triggers the Dual-Aliasing Handle Reuse (0-day logic flaw).
/// It causes data sequence truncation by maliciously interleaving a new message 
/// publication immediately upon receiving a QoS 2 PUBREC, seizing the prematurely recycled packet ID.
#[tokio::main]
async fn main() {
    env_logger::init();
    
    // We connect to a test/dummy local host server (mosquitto etc) to trigger this.
    // E.g., mosquitto -p 1883
    let stream = match TcpStream::connect("127.0.0.1:1883").await {
        Ok(s) => s,
        Err(e) => {
            error!("Could not connect to broker: {:?}", e);
            return;
        }
    };
    
    let mut buffer = [0u8; 4096];
    let mut client = Client::<_, _, 5, 5, 5, 5>::new(&mut buffer);
    let mut options = ConnectOptions::new("trigger_demo_client_1");
    
    if let Err(e) = client.connect(FromTokio::new(stream), &options, None).await {
        error!("Connect failed: {:?}", e);
        return;
    }
    
    info!("Connected successfully. Triggering state machine defect...");

    // 1. Send an initial QoS 2 publish. 
    // This will negotiate a Packet ID, say '1'.
    let mut pub_opt_qos2 = PublicationOptions::new("test/topic");
    pub_opt_qos2.qos = QoS::ExactlyOnce;
    
    match client.publish(&pub_opt_qos2, b"Payload 1 (QoS 2)").await {
        Ok(opt_pid) => info!("Sent QoS 2 publish. Assigned PID: {:?}", opt_pid),
        Err(e) => error!("QoS 2 Publish error: {:?}", e),
    }

    // 2. Poll the event loop to receive PUBREC for PID 1.
    // The moment PUBREC is polled:
    //   a) remove_cpublish("PID 1") is called -> "PID 1" is improperly PUSHED to `free_pids`.
    //   b) await_pubcomp("PID 1") is called -> "PID 1" is mapped to AwaitingPubcomp.
    let event1 = client.poll().await;
    info!("Received event: {:?}", event1); // Expected: PublishReceived (mapped to PUBREC)

    // 3. Immediately send a new message!
    // Because free_pids has "PID 1" in it, packet_identifier() will pop "PID 1" natively.
    // Now both the AwaitingPubcomp and AwaitingPuback messages share "PID 1" in pending_client_publishes vector!
    let mut pub_opt_qos1 = PublicationOptions::new("test/topic2");
    pub_opt_qos1.qos = QoS::AtLeastOnce;
    
    match client.publish(&pub_opt_qos1, b"Payload 2 (QoS 1)").await {
        Ok(opt_pid) => info!("Sent overlapping QoS 1 publish. WRONGLY ASSIGNED RECYCLED PID: {:?}", opt_pid),
        Err(e) => error!("QoS 1 Publish error: {:?}", e),
    }

    info!("Polling again. The server will send a PUBCOMP (for Payload 1).");
    info!("The client will wrongly pop Payload 2 (AwaitingPuback) from the queue and corrupt the transaction flow logic!");
    
    let event2 = client.poll().await;
    info!("Received event 2: {:?}", event2);

    let event3 = client.poll().await;
    info!("Received event 3: {:?}", event3);

    // Demonstration ends. 
    let _ = client.disconnect(&Default::default()).await;
}
