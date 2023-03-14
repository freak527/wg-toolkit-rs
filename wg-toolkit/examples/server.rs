use std::net::{SocketAddr, SocketAddrV4, Ipv4Addr};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use std::sync::Arc;
use std::env;
use std::fs;

use rsa::{RsaPrivateKey, pkcs8::DecodePrivateKey};
use crypto_common::KeyInit;
use blowfish::Blowfish;

use rand::rngs::OsRng;
use rand::RngCore;

use wgtk::net::bundle::{BundleElement, Bundle};
use wgtk::net::app::{App, EventKind, Event};
use wgtk::util::TruncateFmt;

use wgtk::net::element::login::{
    Ping,
    LoginRequest, LoginRequestEncryption,
    LoginResponse, LoginResponseEncryption,
    LoginChallenge, LoginSuccess,
    ChallengeResponse, CuckooCycleResponse,
};

use wgtk::net::element::base::{ClientAuth, ServerSessionKey, ClientSessionKey};

use wgtk::net::element::client::{
    UpdateFrequencyNotification,
    CreateBasePlayer,
    TickSync,
};


fn main() {

    let priv_key_path = env::var("WGTK_PRIVKEY_PATH")
        .expect("Missing 'WGTK_PRIVKEY_PATH' with path to the RSA private key.");

    let bind_ip_raw = env::var("WGTK_BIND_IP")
        .expect("Missing 'WGTK_BIND_IP' with the IP to bind UDP servers.");

    let priv_key_content = fs::read_to_string(priv_key_path).unwrap();
    let priv_key = RsaPrivateKey::from_pkcs8_pem(priv_key_content.as_str()).unwrap();

    let bind_ip: Ipv4Addr = bind_ip_raw.parse().unwrap();

    let mut login_app = LoginApp {
        app: App::new(SocketAddrV4::new(bind_ip, 20016)).unwrap(),
        priv_key: Arc::new(priv_key),
        clients: HashMap::new(),
    };

    let mut base_app = BaseApp {
        app: App::new(SocketAddrV4::new(bind_ip, 20017)).unwrap(),
        pending_clients: HashMap::new(),
        logged_clients: HashMap::new(),
        logged_counter: 0,
        start_time: Instant::now(),
    };

    let mut events = Vec::new();

    loop {
        
        login_app.app.poll(&mut events, Some(Duration::from_millis(10))).unwrap();
        for event in &events {
            login_app.handle(&event, &mut base_app);
        }

        base_app.app.poll(&mut events, Some(Duration::from_millis(10))).unwrap();
        for event in &events {
            base_app.handle(event);
        }

    }

}


/// The state of the login app. This handles login requests, and if validated
/// create a client in the base app and send a success response with the base
/// app address.
pub struct LoginApp {
    /// The application.
    app: App,
    /// The RSA private key for login app.
    priv_key: Arc<RsaPrivateKey>,
    /// A client for the login app.
    clients: HashMap<SocketAddr, LoginClient>,
}

impl LoginApp {

    pub fn handle(&mut self, event: &Event, base_app: &mut BaseApp) {
        match &event.kind {
            EventKind::Bundle(bundle) => {
                let mut reader = bundle.get_element_reader();
                while let Some(element) = reader.next_element() {
                    if !self.handle_element(event.addr, element, &mut *base_app) {
                        break
                    }
                }
            }
            EventKind::PacketError(_, error) => {
                println!("[LOGIN/{}] Packet error: {error:?}", event.addr);
            }
        }
    }

    fn handle_element(&mut self, addr: SocketAddr, element: BundleElement, base_app: &mut BaseApp) -> bool {

        let client = match self.clients.entry(addr) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(LoginClient::new(addr)),
        };

        let prefix = format!("[LOGIN/{}]", client.addr);

        match element {
            BundleElement::Simple(Ping::ID, reader) => {
    
                let elt = reader.read_simple::<Ping>().unwrap();
                println!("{prefix} --> Ping #{}", elt.element.num);
                println!("{prefix} <-- Pong #{}", elt.element.num);
    
                let mut bundle = Bundle::new_empty();
                bundle.add_simple_reply(elt.element, elt.request_id.unwrap());
                self.app.send(&mut bundle, client.addr).unwrap();
                
                true
    
            }
            BundleElement::Simple(LoginRequest::ID, reader) => {
    
                let encryption = LoginRequestEncryption::Server(self.priv_key.clone());
                let elt = reader.read::<LoginRequest>(&encryption).unwrap();

                println!("{prefix} --> Login {} / {}", TruncateFmt(&elt.element.username, 54), elt.element.password);
    
                // Ensure that blowfish key is set.
                let bf = client.blowfish.insert(Arc::new(Blowfish::new_from_slice(&elt.element.blowfish_key).unwrap()));
                let encryption = LoginResponseEncryption::Encrypted(bf.clone());

                let mut bundle = Bundle::new_empty();
                
                if !client.challenge_complete {
                    
                    let cuckoo_prefix_value = OsRng.next_u64();
                    let cuckoo_prefix = format!("{cuckoo_prefix_value:>02X}");
                    let cuckoo_easiness = 0.9;
    
                    let challenge = LoginChallenge::CuckooCycle { 
                        prefix: cuckoo_prefix, 
                        max_nonce: ((1 << 20) as f32 * cuckoo_easiness) as _
                    };
    
                    println!("{prefix} <-- Cuckoo cycle challenge");

                    bundle.add_reply(
                        LoginResponse::Challenge(challenge), 
                        &encryption, 
                        elt.request_id.unwrap()
                    );

                } else {
    
                    // NOTE: We are currently not checking anything prior to connection.
                    // No password, no challenge is required.
    
                    let success = LoginSuccess {
                        addr: base_app.app.addr(),
                        login_key: base_app.alloc_pending_client(client.addr, &*bf),
                        server_message: String::new(),
                    };
    
                    println!("{prefix} <-- Success, addr: {}, login key: {}", success.addr, success.login_key);

                    bundle.add_reply(
                        LoginResponse::Success(success), 
                        &encryption, 
                        elt.request_id.unwrap()
                    );
                    
                }
    
                self.app.send(&mut bundle, client.addr).unwrap();
    
                true
    
            }
            BundleElement::Simple(ChallengeResponse::ID, reader) => {
                let _ = reader.read_simple::<ChallengeResponse<CuckooCycleResponse>>().unwrap();
                println!("{prefix} --> Challenge response");
                client.challenge_complete = true;
                true
            }
            BundleElement::Simple(id, _) => {
                println!("{prefix} --> Unknown #{id}");
                false
            }
            BundleElement::Reply(id, _) => {
                println!("{prefix} --> Unknown reply to #{id}");
                false
            }
        }

    }

}


/// The state of the base app. It is the app where player are playing.
pub struct BaseApp {
    /// Underlying application.
    app: App,
    /// List of clients pending for switching from login app to base app.
    pending_clients: HashMap<u32, PendingBaseClient>,
    /// List of clients logged in the base app mapped to their socket address.
    logged_clients: HashMap<SocketAddr, BaseClient>,
    /// A counter for allocating the unique key for logged Client.
    logged_counter: u32,
    /// Start time of the base app, used to know the game time.
    start_time: Instant,
}

impl BaseApp {

    /// Default update frequency to 10 Hz.
    const UPDATE_FREQ: u8 = 10;

    pub fn handle(&mut self, event: &Event) {

        match &event.kind {
            EventKind::Bundle(bundle) => {
                let mut reader = bundle.get_element_reader();
                while let Some(element) = reader.next_element() {
                    if !self.handle_element(event.addr, element) {
                        break
                    }
                }
            }
            EventKind::PacketError(_, error) => {
                println!("[BASE/{}] Packet error: {error:?}", event.addr);
            }
        }

    }

    fn handle_element(&mut self, addr: SocketAddr, element: BundleElement) -> bool {

        let mut prefix = format!("[BASE/{addr}]");

        let mut logged_client = self.logged_clients.get_mut(&addr);
        if let Some(_) = logged_client.as_deref_mut() {
            prefix.push_str(" (client)");
        }

        match element {
            BundleElement::Simple(ClientAuth::ID, reader) => {

                let client_auth = reader.read_simple::<ClientAuth>().unwrap();

                println!("{prefix} --> Auth, login key: {}, attempt: {}, unk: {}", 
                    client_auth.element.login_key, 
                    client_auth.element.attempts_count,
                    client_auth.element.unk
                );

                if let Some(pending_login) = self.pending_clients.remove(&client_auth.element.login_key) {
                    if pending_login.addr == addr {

                        println!("{prefix}     Enabling channel encryption");
                        self.app.set_channel(addr, pending_login.blowfish);

                        self.logged_counter = self.logged_counter.checked_add(1).expect("too much logged clients");
                        let logged_key = self.logged_counter;

                        self.logged_clients.insert(addr, BaseClient::new(logged_key));

                        // Create a bundle with a single reply.
                        let mut bundle = Bundle::new_empty();
                        bundle.add_simple_reply(ServerSessionKey {
                            session_key: logged_key,
                        }, client_auth.request_id.unwrap());

                        println!("{prefix} <-- Session key: {logged_key}");
                        self.app.send(&mut bundle, addr).unwrap();

                    } else {
                        println!("{prefix}     Incoherent address, expected {}", pending_login.addr);
                    }
                } else {
                    println!("{prefix}     Invalid key, discarding");
                }

                true

            }
            BundleElement::Simple(ClientSessionKey::ID, reader) => {
                
                let client_session_auth = reader.read_simple::<ClientSessionKey>().unwrap();
                let session_key = client_session_auth.element.session_key;

                println!("{prefix} --> Session key: {session_key}");

                if let Some(client) = logged_client.as_deref_mut() {
                    if session_key == client.session_key {
                        if !client.sent_freq {

                            let mut bundle = Bundle::new_empty();
                            bundle.add_simple_element(UpdateFrequencyNotification::ID, UpdateFrequencyNotification {
                                frequency: Self::UPDATE_FREQ,
                                game_time: self.current_time(),
                            });
                            println!("{prefix} <-- Update frequency: {}", Self::UPDATE_FREQ);
                            self.timestamp_bundle(&mut bundle);
                            self.app.send(&mut bundle, addr).unwrap();
                            bundle.clear();
                            
                            self.timestamp_bundle(&mut bundle);
                            bundle.add_simple_element(CreateBasePlayer::ID, CreateBasePlayer {
                                entity_id: 37289213,
                                entity_type: 11,
                                entity_data: b"\x00\x09518858105\x00"[..].into(),
                            });
                            println!("{prefix} <-- Create base player");
                            self.app.send(&mut bundle, addr).unwrap();
                            bundle.clear();

                        }
                    } else {
                        println!("{prefix}     Warning, expected: {}", client.session_key);
                    }
                } else {
                    println!("{prefix}     Warning, no client");
                }

                true

            }
            BundleElement::Simple(id, _) => {
                println!("{prefix} --> Unknown #{id}");
                false
            }
            BundleElement::Reply(id, _) => {
                println!("{prefix} --> Unknown reply to #{id}");
                false
            }
        }

    }

    /// Allocate a new pending client for the given socket address and blowfish key.
    pub fn alloc_pending_client(&mut self, addr: SocketAddr, bf: &Arc<Blowfish>) -> u32 {
        loop {
            let key = OsRng.next_u32();
            match self.pending_clients.entry(key) {
                Entry::Vacant(v) => {
                    v.insert(PendingBaseClient::new(addr, bf.clone()));
                    break key
                }
                _ => continue
            }
        }
    }

    /// Get the current run time of the server in seconds.
    fn current_time(&self) -> u32 {
        self.start_time.elapsed().as_secs() as _
    }

    /// Just wrap around the current time for tick.
    fn current_time_tick(&self) -> u8 {
        self.current_time() as u8
    }

    /// Append a tick sync message to this bundle according to the current time.
    fn timestamp_bundle(&self, bundle: &mut Bundle) {
        bundle.add_simple_element(TickSync::ID, TickSync { 
            tick: self.current_time_tick() 
        });
    }

}


/// Internal structure used to track a client through login process.
#[derive(Debug)]
pub struct LoginClient {
    addr: SocketAddr,
    blowfish: Option<Arc<Blowfish>>,
    challenge_complete: bool,
}

impl LoginClient {

    #[inline]
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            blowfish: None,
            challenge_complete: false,
        }
    }

}

/// Internal structure used to keep track of a client that is switching
/// from the login app after a successful login. 
#[derive(Debug)]
pub struct PendingBaseClient {
    addr: SocketAddr,
    blowfish: Arc<Blowfish>,
}

impl PendingBaseClient {

    #[inline]
    pub fn new(addr: SocketAddr, blowfish: Arc<Blowfish>) -> Self {
        Self { addr, blowfish, }
    }

}

/// Internal structure used to track a client logged in the base app.
#[derive(Debug)]
pub struct BaseClient {
    session_key: u32,
    sent_freq: bool,
}

impl BaseClient {

    #[inline]
    pub fn new(session_key: u32) -> Self {
        Self { 
            session_key, 
            sent_freq: false,
        }
    }

}
