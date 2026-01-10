use quicr::ClientBuilder;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // Create a Quicr client to verfy linkage works
    let _client = ClientBuilder::new()
        .endpoint_id("my-client")
        .connect_uri("moqt://relay.example.com:4433")
        .build()
        .unwrap();
}
