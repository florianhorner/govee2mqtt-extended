use crate::ble::{Base64HexBytes, SetSceneCode};
use crate::opt_env_var;
use crate::platform_api::from_json;
use crate::undoc_api::GoveeUndocumentedApi;
use anyhow::Context;
use if_addrs::IfAddr;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Mutex;
use tokio::time::Instant;

// <https://app-h5.govee.com/user-manual/wlan-guide>

/// The port on which govee devices listen for scan requests
const SCAN_PORT: u16 = 4001;
/// The port on which a client needs to listen to receive responses
/// from govee devices
const LISTEN_PORT: u16 = 4002;
/// The port on which govee devices listen for control requests
const CMD_PORT: u16 = 4003;
/// The multicast group of which govee LAN-API enabled devices are members
const MULTICAST: IpAddr = IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250));

#[derive(clap::Parser, Debug)]
pub struct LanDiscoArguments {
    /// Prevent the use of the default multicast broadcast address.
    /// You may also set GOVEE_LAN_NO_MULTICAST=true via the environment.
    #[arg(long, global = true)]
    pub no_multicast: bool,

    /// Enumerate all interfaces, and for each one that has
    /// a broadcast address, broadcast to it
    /// You may also set GOVEE_LAN_BROADCAST_ALL=true via the environment.
    #[arg(long, global = true)]
    pub broadcast_all: bool,

    /// Broadcast to the global broadcast address 255.255.255.255
    /// You may also set GOVEE_LAN_BROADCAST_GLOBAL=true via the environment.
    #[arg(long, global = true)]
    pub global_broadcast: bool,

    /// Addresses to scan. May be broadcast addresses or individual
    /// IP addresses. Can be specified multiple times.
    /// You may also set GOVEE_LAN_SCAN=10.0.0.1,10.0.0.2 via the environment.
    #[arg(long, global = true)]
    pub scan: Vec<String>,

    /// How long to wait for discovery to complete, in seconds
    /// You may also set GOVEE_LAN_DISCO_TIMEOUT via the environment.
    #[arg(long, default_value_t = 3, global = true)]
    disco_timeout: u64,

    /// How many times to send a status query before giving up.
    /// You may also set GOVEE_LAN_QUERY_ATTEMPTS via the environment.
    #[arg(long, default_value_t = 3, global = true)]
    lan_query_attempts: u64,

    /// Initial wait between status query attempts, in milliseconds.
    /// Doubles on each retry, capped at 3000ms.
    /// You may also set GOVEE_LAN_QUERY_BACKOFF_MS via the environment.
    #[arg(long, default_value_t = 350, global = true)]
    lan_query_backoff_ms: u64,

    /// Number of consecutive status-query timeouts after which periodic
    /// polling of a device is suspended. 0 disables the circuit breaker.
    /// You may also set GOVEE_LAN_BREAKER_THRESHOLD via the environment.
    #[arg(long, default_value_t = 3, global = true)]
    lan_breaker_threshold: u64,

    /// How long to suspend periodic polling of an unresponsive device,
    /// in seconds. Doubles on repeated failure, capped at 900s.
    /// You may also set GOVEE_LAN_BREAKER_COOLDOWN via the environment.
    #[arg(long, default_value_t = 300, global = true)]
    lan_breaker_cooldown: u64,
}

pub fn truthy(s: &str) -> anyhow::Result<bool> {
    if s.eq_ignore_ascii_case("true")
        || s.eq_ignore_ascii_case("yes")
        || s.eq_ignore_ascii_case("on")
        || s == "1"
    {
        Ok(true)
    } else if s.eq_ignore_ascii_case("false")
        || s.eq_ignore_ascii_case("no")
        || s.eq_ignore_ascii_case("off")
        || s == "0"
    {
        Ok(false)
    } else {
        anyhow::bail!("invalid value '{s}', expected true/yes/on/1 or false/no/off/0");
    }
}

impl LanDiscoArguments {
    pub fn to_disco_options(&self) -> anyhow::Result<DiscoOptions> {
        let mut scan_names = self.scan.clone();
        let mut options = DiscoOptions {
            enable_multicast: !self.no_multicast,
            additional_addresses: vec![],
            broadcast_all_interfaces: self.broadcast_all,
            global_broadcast: self.global_broadcast,
            query_policy: LanQueryPolicy {
                attempts: self.lan_query_attempts.clamp(1, u32::MAX as u64) as u32,
                initial_backoff: Duration::from_millis(self.lan_query_backoff_ms),
            },
            breaker: BreakerConfig {
                // min before cast: a threshold beyond u32 must saturate,
                // not truncate to 0 and silently disable the breaker
                threshold: self.lan_breaker_threshold.min(u32::MAX as u64) as u32,
                base_cooldown: Duration::from_secs(self.lan_breaker_cooldown),
            },
        };

        if let Some(v) = opt_env_var::<u64>("GOVEE_LAN_QUERY_ATTEMPTS")? {
            options.query_policy.attempts = v.clamp(1, u32::MAX as u64) as u32;
        }

        if let Some(v) = opt_env_var::<u64>("GOVEE_LAN_QUERY_BACKOFF_MS")? {
            options.query_policy.initial_backoff = Duration::from_millis(v);
        }

        if let Some(v) = opt_env_var::<u64>("GOVEE_LAN_BREAKER_THRESHOLD")? {
            options.breaker.threshold = v.min(u32::MAX as u64) as u32;
        }

        if let Some(v) = opt_env_var::<u64>("GOVEE_LAN_BREAKER_COOLDOWN")? {
            options.breaker.base_cooldown = Duration::from_secs(v);
        }

        if let Some(v) = opt_env_var::<String>("GOVEE_LAN_NO_MULTICAST")? {
            options.enable_multicast = !truthy(&v)?;
        }

        if let Some(v) = opt_env_var::<String>("GOVEE_LAN_BROADCAST_ALL")? {
            options.broadcast_all_interfaces = truthy(&v)?;
        }

        if let Some(v) = opt_env_var::<String>("GOVEE_LAN_BROADCAST_GLOBAL")? {
            options.global_broadcast = truthy(&v)?;
        }

        if let Some(v) = opt_env_var::<String>("GOVEE_LAN_SCAN")? {
            for addr in v.split(',') {
                scan_names.push(addr.trim().to_string());
            }
        }

        for name in scan_names {
            match name.parse::<IpAddr>() {
                Ok(addr) => {
                    options.additional_addresses.push(addr);
                }
                Err(err1) => match (name.as_str(), SCAN_PORT).to_socket_addrs() {
                    Ok(addrs) => {
                        for a in addrs {
                            options.additional_addresses.push(a.ip());
                        }
                    }
                    Err(err2) => {
                        anyhow::bail!(
                            "{name} could not be parsed as either an \
                            IpAddr ({err1:#}) or a DNS name ({err2:#}"
                        );
                    }
                },
            }
        }

        Ok(options)
    }

    pub fn disco_timeout(&self) -> anyhow::Result<u64> {
        if let Some(v) = opt_env_var("GOVEE_LAN_DISCO_TIMEOUT")? {
            Ok(v)
        } else {
            Ok(self.disco_timeout)
        }
    }
}

pub struct DiscoOptions {
    /// Use the MULTICAST address defined in the LAN protocol
    pub enable_multicast: bool,
    /// Send to this list of additional addresses, which may
    /// include multicast addresses
    pub additional_addresses: Vec<IpAddr>,
    /// Enumerate all interfaces, and for each one that has
    /// a broadcast address, broadcast to it
    pub broadcast_all_interfaces: bool,
    /// Broadcast to the global broadcast address
    pub global_broadcast: bool,
    /// Retry schedule for device status queries
    pub query_policy: LanQueryPolicy,
    /// Circuit breaker configuration for periodic polling
    pub breaker: BreakerConfig,
}

impl DiscoOptions {
    pub fn is_empty(&self) -> bool {
        !self.enable_multicast
            && self.additional_addresses.is_empty()
            && !self.broadcast_all_interfaces
            && !self.global_broadcast
    }
}

impl Default for DiscoOptions {
    fn default() -> Self {
        Self {
            enable_multicast: true,
            additional_addresses: vec![],
            broadcast_all_interfaces: false,
            global_broadcast: false,
            query_policy: LanQueryPolicy::default(),
            breaker: BreakerConfig::default(),
        }
    }
}

/// Each retry wait doubles, capped here. Not configurable: two knobs
/// (attempts + initial backoff) describe the schedule fully.
const QUERY_BACKOFF_CAP: Duration = Duration::from_millis(3000);
/// Breaker cooldown doubling stops here.
const BREAKER_COOLDOWN_CAP: Duration = Duration::from_secs(900);
/// Below this a cooldown is churn: the breaker would flip
/// open->probe on every poll tick, logging each time and
/// suppressing nothing.
const BREAKER_COOLDOWN_MIN: Duration = Duration::from_secs(30);
/// A half-open probe whose outcome never arrives (task cancelled,
/// send error) must not wedge the breaker; after this long a new
/// probe is granted. Deliberately NOT equal to the 30s poll period,
/// so scheduler jitter can't decide which tick self-heals.
const HALF_OPEN_PROBE_EXPIRY: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanQueryPolicy {
    pub attempts: u32,
    pub initial_backoff: Duration,
}

impl Default for LanQueryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            initial_backoff: Duration::from_millis(350),
        }
    }
}

impl LanQueryPolicy {
    /// More attempts than this is config nonsense; the clamp also
    /// bounds the schedule allocation against absurd env values.
    const MAX_ATTEMPTS: u32 = 100;
    /// A zero backoff would send the whole schedule back-to-back,
    /// defeating the spacing this policy exists to provide.
    const MIN_BACKOFF: Duration = Duration::from_millis(10);

    /// The wait after each send. One entry per attempt, doubling
    /// up to QUERY_BACKOFF_CAP. Always at least one attempt.
    pub fn schedule(&self) -> Vec<Duration> {
        let attempts = self.attempts.clamp(1, Self::MAX_ATTEMPTS);
        let mut waits = Vec::with_capacity(attempts as usize);
        let mut wait = self
            .initial_backoff
            .clamp(Self::MIN_BACKOFF, QUERY_BACKOFF_CAP);
        for _ in 0..attempts {
            waits.push(wait);
            wait = (wait * 2).min(QUERY_BACKOFF_CAP);
        }
        waits
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakerConfig {
    /// Consecutive failures before opening. 0 disables the breaker.
    pub threshold: u32,
    pub base_cooldown: Duration,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            threshold: 3,
            base_cooldown: Duration::from_secs(300),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakerState {
    Closed,
    Open { until: Instant, cooldown: Duration },
    HalfOpen { since: Instant, cooldown: Duration },
}

#[derive(Debug)]
struct BreakerEntry {
    consecutive_failures: u32,
    state: BreakerState,
}

/// Per-IP circuit breaker over an injected clock. Only the periodic
/// poll path consults `check`; post-command confirm polls bypass it so
/// a successful LAN command always produces a state echo for HA, but
/// their outcomes still feed the failure counter — unreachability
/// evidence is free signal regardless of caller.
pub struct BreakerCore {
    config: BreakerConfig,
    /// At least HALF_OPEN_PROBE_EXPIRY, raised to cover the full
    /// query schedule: a probe legitimately in flight for the whole
    /// schedule must not have its slot reclaimed mid-probe, or two
    /// probes run concurrently.
    probe_expiry: Duration,
    by_ip: HashMap<IpAddr, BreakerEntry>,
}

impl BreakerCore {
    pub fn with_policy(mut config: BreakerConfig, policy: &LanQueryPolicy) -> Self {
        // Clamp the configured base into [30s, 900s]: the cap keeps
        // the documented 900s promise and prevents an absurd value
        // from overflowing `now + cooldown`; the floor keeps a tiny
        // cooldown from churning open->probe on every poll tick.
        config.base_cooldown = config
            .base_cooldown
            .clamp(BREAKER_COOLDOWN_MIN, BREAKER_COOLDOWN_CAP);
        // A large attempts config makes a legitimate probe outlive
        // the default expiry; give it the whole schedule plus margin.
        let schedule_total: Duration = policy.schedule().iter().sum();
        let probe_expiry = HALF_OPEN_PROBE_EXPIRY.max(schedule_total + Duration::from_secs(15));
        Self {
            config,
            probe_expiry,
            by_ip: HashMap::new(),
        }
    }

    fn disabled(&self) -> bool {
        self.config.threshold == 0
    }

    /// May the gated caller send to this device right now?
    /// Granting from Open/HalfOpen consumes the single probe slot.
    pub fn check(&mut self, ip: IpAddr, now: Instant) -> bool {
        if self.disabled() {
            return true;
        }
        let Some(entry) = self.by_ip.get_mut(&ip) else {
            return true;
        };
        match entry.state {
            BreakerState::Closed => true,
            BreakerState::Open { until, cooldown } => {
                if now < until {
                    false
                } else {
                    log::info!("LAN breaker for {ip}: cooldown expired, allowing one probe");
                    entry.state = BreakerState::HalfOpen {
                        since: now,
                        cooldown,
                    };
                    true
                }
            }
            BreakerState::HalfOpen { since, cooldown } => {
                if now.saturating_duration_since(since) > self.probe_expiry {
                    log::info!(
                        "LAN breaker for {ip}: prior probe never resolved, allowing another"
                    );
                    entry.state = BreakerState::HalfOpen {
                        since: now,
                        cooldown,
                    };
                    true
                } else {
                    false
                }
            }
        }
    }

    /// A real devStatus reply: the device is fully back. Clears the
    /// failure count and any open state.
    pub fn on_success(&mut self, ip: IpAddr) {
        if self.disabled() {
            return;
        }
        if let Some(entry) = self.by_ip.remove(&ip) {
            if !matches!(entry.state, BreakerState::Closed) {
                log::info!("LAN breaker for {ip}: device responded, closing");
            }
        }
    }

    /// Any other parseable packet from the device (typically a
    /// discovery-scan reply). Weaker evidence than a devStatus reply:
    /// it shortcuts an open cooldown so the next gated caller probes
    /// immediately, but it does NOT clear the failure count or close
    /// the breaker. If it did, a device that answers broadcast scans
    /// (every ~60s) while dropping unicast status queries would reset
    /// the counter faster than the 30s poll can accrue it, and the
    /// breaker could never open for exactly the devices it targets.
    pub fn note_packet(&mut self, ip: IpAddr, now: Instant) {
        if self.disabled() {
            return;
        }
        if let Some(entry) = self.by_ip.get_mut(&ip) {
            if let BreakerState::Open { until, cooldown } = entry.state {
                if now < until {
                    log::info!("LAN breaker for {ip}: device shows life, allowing early probe");
                    entry.state = BreakerState::Open {
                        until: now,
                        cooldown,
                    };
                }
            }
        }
    }

    pub fn on_failure(&mut self, ip: IpAddr, now: Instant) {
        if self.disabled() {
            return;
        }
        let entry = self.by_ip.entry(ip).or_insert(BreakerEntry {
            consecutive_failures: 0,
            state: BreakerState::Closed,
        });
        entry.consecutive_failures += 1;
        match entry.state {
            BreakerState::Closed => {
                if entry.consecutive_failures >= self.config.threshold {
                    let cooldown = self.config.base_cooldown;
                    log::info!(
                        "LAN breaker for {ip}: {} consecutive timeouts, \
                        suspending periodic polls for {cooldown:?}",
                        entry.consecutive_failures
                    );
                    entry.state = BreakerState::Open {
                        until: now + cooldown,
                        cooldown,
                    };
                }
            }
            BreakerState::HalfOpen { cooldown, .. } => {
                let cooldown = (cooldown * 2).min(BREAKER_COOLDOWN_CAP);
                log::info!(
                    "LAN breaker for {ip}: probe failed, \
                    suspending periodic polls for {cooldown:?}"
                );
                entry.state = BreakerState::Open {
                    until: now + cooldown,
                    cooldown,
                };
            }
            // Ungated confirm polls can fail while already open;
            // the count was recorded above, nothing else to do.
            BreakerState::Open { .. } => {}
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "cmd", content = "data")]
pub enum Request {
    #[serde(rename = "scan")]
    Scan { account_topic: AccountTopic },
    #[serde(rename = "devStatus")]
    DevStatus {},
    #[serde(rename = "turn")]
    Turn { value: u8 },
    #[serde(rename = "brightness")]
    Brightness { value: u8 },
    #[serde(rename = "colorwc")]
    Color {
        color: DeviceColor,
        #[serde(rename = "colorTemInKelvin")]
        color_temperature_kelvin: u32,
    },
    #[serde(rename = "ptReal")]
    PtReal { command: Vec<String> },
}

#[derive(Serialize, Deserialize, Debug)]
struct RequestMessage {
    msg: Request,
}

fn unspec_ip() -> IpAddr {
    Ipv4Addr::UNSPECIFIED.into()
}

#[derive(Serialize, Deserialize, Debug, Clone, Hash, Eq, PartialEq)]
pub struct LanDevice {
    #[serde(default = "unspec_ip")]
    pub ip: IpAddr,
    pub device: String,
    pub sku: String,
    #[serde(rename = "bleVersionHard")]
    pub ble_version_hard: String,
    #[serde(rename = "bleVersionSoft")]
    pub ble_version_soft: String,
    #[serde(rename = "wifiVersionHard")]
    pub wifi_version_hard: String,
    #[serde(rename = "wifiVersionSoft")]
    pub wifi_version_soft: String,
}

impl LanDevice {
    pub async fn send_request(&self, msg: Request) -> anyhow::Result<()> {
        log::trace!("LanDevice::send_request to {:?} {msg:?}", self.ip);
        let client = udp_socket_for_target(self.ip).await?;
        let data = serde_json::to_string(&RequestMessage { msg })?;
        client.send_to(data.as_bytes(), (self.ip, CMD_PORT)).await?;

        Ok(())
    }

    pub async fn send_turn(&self, on: bool) -> anyhow::Result<()> {
        self.send_request(Request::Turn {
            value: if on { 1 } else { 0 },
        })
        .await
    }

    pub async fn send_brightness(&self, percent: u8) -> anyhow::Result<()> {
        self.send_request(Request::Brightness { value: percent })
            .await
    }

    pub async fn send_color_rgb(&self, color: DeviceColor) -> anyhow::Result<()> {
        self.send_request(Request::Color {
            color,
            color_temperature_kelvin: 0,
        })
        .await
    }

    pub async fn send_real(&self, commands: Vec<String>) -> anyhow::Result<()> {
        self.send_request(Request::PtReal { command: commands })
            .await
    }

    pub async fn send_color_temperature_kelvin(
        &self,
        color_temperature_kelvin: u32,
    ) -> anyhow::Result<()> {
        self.send_request(Request::Color {
            color: DeviceColor { r: 0, g: 0, b: 0 },
            color_temperature_kelvin,
        })
        .await
    }

    pub async fn set_scene_by_name(&self, scene_name: &str) -> anyhow::Result<()> {
        for category in GoveeUndocumentedApi::get_scenes_for_device(&self.sku).await? {
            for scene in category.scenes {
                for effect in scene.light_effects {
                    if scene.scene_name == scene_name && effect.scene_code != 0 {
                        let encoded = Base64HexBytes::encode_for_sku(
                            "Generic:Light",
                            &SetSceneCode::new(effect.scene_code, effect.scence_param),
                        )?
                        .base64();
                        log::info!(
                            "sending scene packet {encoded:x?} for {scene_name}, code {}",
                            effect.scene_code
                        );
                        return self.send_real(encoded).await;
                    }
                }
            }
        }

        anyhow::bail!("unable to set scene {scene_name} for {}", self.device);
    }
}

pub fn boolean_int<'de, D: serde::de::Deserializer<'de>>(
    deserializer: D,
) -> Result<bool, D::Error> {
    Ok(match serde::de::Deserialize::deserialize(deserializer)? {
        JsonValue::Bool(b) => b,
        JsonValue::Number(num) => {
            num.as_i64()
                .ok_or(serde::de::Error::custom("Invalid number"))?
                != 0
        }
        JsonValue::Null => false,
        _ => return Err(serde::de::Error::custom("Wrong type, expected boolean")),
    })
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceStatus {
    #[serde(rename = "onOff", deserialize_with = "boolean_int")]
    pub on: bool,
    pub brightness: u8,
    pub color: DeviceColor,
    #[serde(rename = "colorTemInKelvin")]
    pub color_temperature_kelvin: u32,
    /// Active work mode number. Reported by the AWS IoT status
    /// message; not present in LAN devStatus responses.
    /// The meaning of the number varies by SKU (eg: H607C reports
    /// 5 for manual color and 4 for music mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "cmd", content = "data")]
pub enum Response {
    #[serde(rename = "scan")]
    Scan(LanDevice),
    #[serde(rename = "devStatus")]
    DevStatus(DeviceStatus),
}

#[derive(Serialize, Deserialize, Debug)]
struct ResponseWrapper {
    msg: Response,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum AccountTopic {
    #[serde(rename = "reserve")]
    Reserve,
}

struct ClientListener {
    addr: IpAddr,
    tx: Sender<Response>,
}

struct ClientInner {
    mux: Mutex<Vec<ClientListener>>,
    query_policy: LanQueryPolicy,
    // std Mutex, never held across an await: lock scopes are a few
    // HashMap operations.
    breaker: StdMutex<BreakerCore>,
}

impl ClientInner {
    fn new(options: &DiscoOptions) -> Self {
        Self {
            mux: Mutex::new(vec![]),
            query_policy: options.query_policy.clone(),
            breaker: StdMutex::new(BreakerCore::with_policy(
                options.breaker.clone(),
                &options.query_policy,
            )),
        }
    }

    /// Poison recovery instead of unwrap: this mutex is shared by the
    /// disco task and every query; a poisoned lock must degrade to
    /// slightly-stale breaker state, not take down the LAN subsystem.
    fn breaker_lock(&self) -> std::sync::MutexGuard<'_, BreakerCore> {
        self.breaker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn breaker_check(&self, ip: IpAddr, now: Instant) -> bool {
        self.breaker_lock().check(ip, now)
    }

    fn breaker_success(&self, ip: IpAddr) {
        self.breaker_lock().on_success(ip);
    }

    fn breaker_note_packet(&self, ip: IpAddr, now: Instant) {
        self.breaker_lock().note_packet(ip, now);
    }

    fn breaker_failure(&self, ip: IpAddr, now: Instant) {
        self.breaker_lock().on_failure(ip, now);
    }
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

#[derive(Debug)]
struct Broadcaster {
    addr: IpAddr,
    socket: UdpSocket,
}

async fn udp_socket_for_target(addr: IpAddr) -> std::io::Result<UdpSocket> {
    match addr {
        IpAddr::V4(_) => UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await,
        IpAddr::V6(_) => UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await,
    }
}

impl Broadcaster {
    pub async fn new(addr: IpAddr) -> std::io::Result<Self> {
        let socket = udp_socket_for_target(addr).await?;

        if addr.is_multicast() {
            match addr {
                IpAddr::V4(v4) => {
                    socket.join_multicast_v4(v4, Ipv4Addr::UNSPECIFIED)?;
                    socket.set_multicast_loop_v4(false)?;
                }
                IpAddr::V6(v6) => {
                    socket.join_multicast_v6(&v6, 0)?;
                    socket.set_multicast_loop_v6(false)?;
                }
            }
        } else {
            socket.set_broadcast(true)?;
        }

        Ok(Self { addr, socket })
    }

    pub async fn broadcast<B: AsRef<[u8]>>(&self, bytes: B) -> std::io::Result<()> {
        self.socket
            .send_to(bytes.as_ref(), (self.addr, SCAN_PORT))
            .await?;
        Ok(())
    }
}

async fn send_scan(options: &DiscoOptions) -> anyhow::Result<()> {
    let mut addresses = options.additional_addresses.clone();
    if options.enable_multicast {
        addresses.push(MULTICAST);
    }
    if options.global_broadcast {
        addresses.push(Ipv4Addr::BROADCAST.into());
    }
    if options.broadcast_all_interfaces {
        match if_addrs::get_if_addrs() {
            Ok(ifaces) => {
                for iface in ifaces {
                    if iface.is_loopback() {
                        continue;
                    }
                    let bcast = match iface.addr {
                        IfAddr::V4(v4) => v4.broadcast.map(IpAddr::V4),
                        IfAddr::V6(v6) => v6.broadcast.map(IpAddr::V6),
                    };
                    if let Some(addr) = bcast {
                        log::debug!("Adding bcast {addr} from if {}", iface.name);
                        addresses.push(addr);
                    }
                }
            }
            Err(err) => {
                log::error!("get_if_addrs: {err:#}");
            }
        }
    }

    let mut broadcasters = vec![];
    for addr in addresses {
        match Broadcaster::new(addr).await {
            Ok(b) => broadcasters.push(b),
            Err(err) => {
                log::error!("{addr}: {err:#}");
            }
        }
    }

    let scan = serde_json::to_string(&RequestMessage {
        msg: Request::Scan {
            account_topic: AccountTopic::Reserve,
        },
    })
    .expect("to serialize scan message");
    for b in broadcasters {
        log::trace!("Send disco packet to {:?}", b.addr);
        if let Err(err) = b.broadcast(&scan).await {
            log::error!("Error broadcasting to {b:?}: {err:#}");
        }
    }
    Ok(())
}

async fn lan_disco(
    options: DiscoOptions,
    inner: Arc<ClientInner>,
) -> anyhow::Result<Receiver<LanDevice>> {
    let listen = UdpSocket::bind(("0.0.0.0", LISTEN_PORT)).await.context(
        "Cannot bind to UDP Port 4002, which is required \
        for the Govee LAN API to function. Most likely cause is that you \
        are running another integration (perhaps `Govee LAN Control`, or \
        `homebridge-govee`) that is already bound to that port. \
        Both cannot run on the same machine at the same time. \
        Consider disabling `Govee LAN Control` or setting `lanDisable` in \
        `homebridge-govee`.",
    )?;
    let (tx, rx) = channel(8);

    async fn process_packet(
        addr: SocketAddr,
        data: &[u8],
        inner: &Arc<ClientInner>,
        tx: &Sender<LanDevice>,
    ) -> anyhow::Result<()> {
        log::trace!(
            "process_packet: addr={addr:?} data={}",
            String::from_utf8_lossy(data)
        );

        let mut response: ResponseWrapper = from_json(data)
            .with_context(|| format!("Parsing: {}", String::from_utf8_lossy(data)))?;

        // Any parseable packet from this address is evidence of life;
        // it shortcuts an open breaker's cooldown so the disco-triggered
        // gated query below (or the next poll tick) probes immediately.
        // Only a real devStatus reply fully resets the breaker.
        inner.breaker_note_packet(addr.ip(), Instant::now());

        // This is frustrating; some newer devices don't emit the ip field
        // as defined in the spec.  What we do to deal with this is default
        // the ip to the v4 unspecified address during deserialization and
        // check for it here. We'll assume that the ip to use is the ip
        // from which we got the scan response.
        // <https://github.com/wez/govee2mqtt/issues/437>
        if let Response::Scan(dev) = &mut response.msg {
            if dev.ip.is_unspecified() {
                dev.ip = addr.ip();
            } else if dev.ip != addr.ip() {
                log::warn!(
                    "Got scan packet from {addr:?} which has ip set to a different device {dev:?}"
                );
            }
        }

        let mut mux = inner.mux.lock().await;
        mux.retain(|l| !l.tx.is_closed());
        for l in mux.iter() {
            if l.addr == addr.ip() {
                l.tx.send(response.msg.clone()).await.ok();
            }
        }

        if let Response::Scan(info) = response.msg {
            tx.send(info).await?;
        }

        Ok(())
    }

    async fn run_disco(
        options: &DiscoOptions,
        listen: UdpSocket,
        tx: Sender<LanDevice>,
        inner: Arc<ClientInner>,
    ) -> anyhow::Result<()> {
        send_scan(options).await?;

        let mut retry_interval = Duration::from_secs(2);
        let max_retry = Duration::from_secs(60);
        let mut last_send = Instant::now();
        loop {
            let mut buf = [0u8; 4096];

            let deadline = last_send + retry_interval;
            match tokio::time::timeout_at(deadline, listen.recv_from(&mut buf)).await {
                Ok(Ok((len, addr))) => {
                    if let Err(err) = process_packet(addr, &buf[0..len], &inner, &tx).await {
                        log::error!("process_packet: {err:#}");
                    }
                }
                Ok(Err(err)) => {
                    log::error!("recv_from: {err:#}");
                }
                Err(_) => {
                    send_scan(options).await?;
                    last_send = Instant::now();
                    retry_interval = (retry_interval * 2).min(max_retry);
                }
            }
        }
    }

    tokio::spawn(async move {
        if let Err(err) = run_disco(&options, listen, tx, inner).await {
            log::error!("Error at the disco: {err:#}");
        }
    });

    Ok(rx)
}

impl Client {
    pub async fn new(options: DiscoOptions) -> anyhow::Result<(Self, Receiver<LanDevice>)> {
        let inner = Arc::new(ClientInner::new(&options));
        let rx = lan_disco(options, Arc::clone(&inner)).await?;

        Ok((Self { inner }, rx))
    }

    async fn add_listener(&self, addr: IpAddr) -> anyhow::Result<Receiver<Response>> {
        let (tx, rx) = channel(1);
        let mut mux = self.inner.mux.lock().await;
        mux.push(ClientListener { addr, tx });
        Ok(rx)
    }

    /// Interrogate `addr` by sending a scan request to it.
    /// If it is a Govee device that supports the lan protocol,
    /// this method will yield a LanDevice representing it.
    /// In addition, its details will be routed via the discovery
    /// receiver.
    pub async fn scan_ip(&self, addr: IpAddr) -> anyhow::Result<LanDevice> {
        let mut rx = self.add_listener(addr).await?;

        let bcast = Broadcaster::new(addr).await?;
        let scan = serde_json::to_string(&RequestMessage {
            msg: Request::Scan {
                account_topic: AccountTopic::Reserve,
            },
        })
        .expect("to serialize scan message");
        bcast.broadcast(scan).await?;

        loop {
            match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
                Ok(Some(Response::Scan(info))) => {
                    return Ok(info);
                }
                Ok(Some(_)) => {}
                Ok(None) => anyhow::bail!("listener thread terminated"),
                Err(_) => anyhow::bail!("timeout waiting for response"),
            }
        }
    }

    /// Query device status. This is the ungated path used by
    /// post-command confirm polls: it always sends (bounded by the
    /// retry schedule) so HA gets a state echo after a LAN command,
    /// but its outcome still feeds the circuit breaker.
    pub async fn query_status(&self, device: &LanDevice) -> anyhow::Result<DeviceStatus> {
        self.query_status_impl(device).await
    }

    /// Query device status, but refuse without sending a packet while
    /// the device's circuit breaker is open. Used by the periodic
    /// poll and discovery paths, where per-device volume lives.
    pub async fn query_status_respecting_breaker(
        &self,
        device: &LanDevice,
    ) -> anyhow::Result<DeviceStatus> {
        if !self.inner.breaker_check(device.ip, Instant::now()) {
            anyhow::bail!(
                "circuit breaker open for {}: skipping LAN status query",
                device.ip
            );
        }
        self.query_status_impl(device).await
    }

    async fn query_status_impl(&self, device: &LanDevice) -> anyhow::Result<DeviceStatus> {
        let mut rx = self.add_listener(device.ip).await?;
        for wait in self.inner.query_policy.schedule() {
            log::trace!("query status of {}", device.ip);
            if let Err(err) = device.send_request(Request::DevStatus {}).await {
                // A send error (e.g. a queued ICMP host-unreachable
                // surfacing on the next send) is unreachability
                // evidence too; without this a hard-down device
                // would never trip the breaker, and a lost half-open
                // probe would wedge until the probe expiry.
                self.inner.breaker_failure(device.ip, Instant::now());
                return Err(err);
            }
            let deadline = Instant::now() + wait;
            // Non-status packets from this device must not consume
            // the attempt's wait budget.
            loop {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(Response::DevStatus(status))) => {
                        self.inner.breaker_success(device.ip);
                        return Ok(status);
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => anyhow::bail!("listener thread terminated"),
                    Err(_) => break,
                }
            }
        }

        self.inner.breaker_failure(device.ip, Instant::now());
        anyhow::bail!("timed out waiting for status from {}", device.ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);
    const SEC: Duration = Duration::from_secs(1);

    fn ip() -> IpAddr {
        "192.168.1.50".parse().unwrap()
    }

    fn breaker(threshold: u32, base_cooldown: Duration) -> BreakerCore {
        BreakerCore::with_policy(
            BreakerConfig {
                threshold,
                base_cooldown,
            },
            &LanQueryPolicy::default(),
        )
    }

    // -- LanQueryPolicy --

    #[test]
    fn default_schedule_doubles_from_350ms() {
        let waits = LanQueryPolicy::default().schedule();
        assert_eq!(
            waits,
            vec![350 * MS, 700 * MS, 1400 * MS],
            "3 attempts doubling from 350ms"
        );
    }

    #[test]
    fn schedule_budget_fits_under_confirm_poll_deadline() {
        // poll_lan_api in state.rs loops on a 5s outer deadline;
        // a full retry cycle must fit inside it.
        let total: Duration = LanQueryPolicy::default().schedule().iter().sum();
        assert!(total < Duration::from_secs(5), "total {total:?}");
    }

    #[test]
    fn schedule_caps_backoff_growth() {
        let waits = LanQueryPolicy {
            attempts: 6,
            initial_backoff: 350 * MS,
        }
        .schedule();
        assert_eq!(waits.len(), 6);
        assert!(waits.iter().all(|w| *w <= QUERY_BACKOFF_CAP));
        assert_eq!(*waits.last().unwrap(), QUERY_BACKOFF_CAP);
    }

    #[test]
    fn schedule_single_attempt() {
        let waits = LanQueryPolicy {
            attempts: 1,
            initial_backoff: 350 * MS,
        }
        .schedule();
        assert_eq!(waits, vec![350 * MS]);
    }

    #[test]
    fn schedule_zero_attempts_clamps_to_one() {
        // attempts=0 must not produce an empty schedule that would
        // fail without ever sending a packet.
        let waits = LanQueryPolicy {
            attempts: 0,
            initial_backoff: 350 * MS,
        }
        .schedule();
        assert_eq!(waits.len(), 1);
    }

    #[test]
    fn initial_backoff_above_cap_is_clamped() {
        let waits = LanQueryPolicy {
            attempts: 2,
            initial_backoff: 10 * SEC,
        }
        .schedule();
        assert_eq!(waits, vec![QUERY_BACKOFF_CAP, QUERY_BACKOFF_CAP]);
    }

    #[test]
    fn absurd_attempts_value_cannot_exhaust_memory() {
        // A typo'd GOVEE_LAN_QUERY_ATTEMPTS must not drive a
        // multi-gigabyte Vec allocation.
        let waits = LanQueryPolicy {
            attempts: u32::MAX,
            initial_backoff: 350 * MS,
        }
        .schedule();
        assert_eq!(waits.len(), LanQueryPolicy::MAX_ATTEMPTS as usize);
    }

    #[test]
    fn zero_backoff_is_floored() {
        // backoff=0 would send the whole schedule back-to-back,
        // defeating the retransmit spacing entirely.
        let waits = LanQueryPolicy {
            attempts: 3,
            initial_backoff: Duration::ZERO,
        }
        .schedule();
        assert!(waits.iter().all(|w| *w >= LanQueryPolicy::MIN_BACKOFF));
    }

    fn disco_args(
        attempts: u64,
        backoff_ms: u64,
        threshold: u64,
        cooldown: u64,
    ) -> LanDiscoArguments {
        LanDiscoArguments {
            no_multicast: false,
            broadcast_all: false,
            global_broadcast: false,
            scan: vec![],
            disco_timeout: 3,
            lan_query_attempts: attempts,
            lan_query_backoff_ms: backoff_ms,
            lan_breaker_threshold: threshold,
            lan_breaker_cooldown: cooldown,
        }
    }

    // CLI-path and env-path assertions share one test because both
    // read the same process-global env vars; two parallel tests
    // would race on them.
    #[test]
    fn config_plumbing_cli_and_env_precedence() {
        // Guarantee a clean slate: to_disco_options reads all of
        // these, so any ambient value would poison phase 1 (an
        // ambient GOVEE_LAN_SCAN can even fail DNS parsing).
        for var in [
            "GOVEE_LAN_QUERY_ATTEMPTS",
            "GOVEE_LAN_QUERY_BACKOFF_MS",
            "GOVEE_LAN_BREAKER_THRESHOLD",
            "GOVEE_LAN_BREAKER_COOLDOWN",
            "GOVEE_LAN_NO_MULTICAST",
            "GOVEE_LAN_BROADCAST_ALL",
            "GOVEE_LAN_BROADCAST_GLOBAL",
            "GOVEE_LAN_SCAN",
        ] {
            std::env::remove_var(var);
        }

        // Phase 1 — no env set: CLI values flow through, and a value
        // beyond u32 saturates instead of truncating to 0 (which
        // would silently mean "1 attempt").
        let options = disco_args(u64::from(u32::MAX) + 1, 350, 3, 300)
            .to_disco_options()
            .expect("options");
        assert_eq!(options.query_policy.attempts, u32::MAX);
        assert_eq!(
            options.query_policy.schedule().len(),
            LanQueryPolicy::MAX_ATTEMPTS as usize
        );

        // Phase 2 — env set: env beats CLI, absurd values clamp.
        std::env::set_var("GOVEE_LAN_QUERY_ATTEMPTS", "0");
        std::env::set_var("GOVEE_LAN_QUERY_BACKOFF_MS", "50");
        std::env::set_var("GOVEE_LAN_BREAKER_THRESHOLD", "99999999999");
        std::env::set_var("GOVEE_LAN_BREAKER_COOLDOWN", "120");
        let options = disco_args(7, 700, 5, 500)
            .to_disco_options()
            .expect("options");
        std::env::remove_var("GOVEE_LAN_QUERY_ATTEMPTS");
        std::env::remove_var("GOVEE_LAN_QUERY_BACKOFF_MS");
        std::env::remove_var("GOVEE_LAN_BREAKER_THRESHOLD");
        std::env::remove_var("GOVEE_LAN_BREAKER_COOLDOWN");
        assert_eq!(options.query_policy.attempts, 1, "0 clamps to 1");
        assert_eq!(options.query_policy.initial_backoff, 50 * MS);
        assert_eq!(
            options.breaker.threshold,
            u32::MAX,
            "saturates, not truncates"
        );
        assert_eq!(options.breaker.base_cooldown, 120 * SEC);
    }

    #[test]
    fn absurd_cooldown_is_clamped_to_cap() {
        // An absurd GOVEE_LAN_BREAKER_COOLDOWN must not overflow
        // `now + cooldown` when the breaker opens.
        let mut b = breaker(3, Duration::from_secs(u64::MAX));
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        assert!(!b.check(ip(), now + 899 * SEC));
        assert!(b.check(ip(), now + 901 * SEC));
    }

    // -- BreakerCore transitions --

    #[test]
    fn below_threshold_stays_closed() {
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        b.on_failure(ip(), now);
        b.on_failure(ip(), now);
        assert!(b.check(ip(), now));
    }

    #[test]
    fn nth_consecutive_failure_opens() {
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        assert!(!b.check(ip(), now));
    }

    #[test]
    fn open_refuses_before_cooldown_expiry() {
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        assert!(!b.check(ip(), now + 299 * SEC));
    }

    #[test]
    fn cooldown_expiry_grants_exactly_one_probe() {
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        let later = now + 301 * SEC;
        assert!(b.check(ip(), later), "first caller gets the probe");
        assert!(!b.check(ip(), later), "second caller is refused");
    }

    #[test]
    fn probe_success_closes_and_resets() {
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        let later = now + 301 * SEC;
        assert!(b.check(ip(), later));
        b.on_success(ip());
        assert!(b.check(ip(), later));
        // failure count was reset: two failures don't re-open
        b.on_failure(ip(), later);
        b.on_failure(ip(), later);
        assert!(b.check(ip(), later));
    }

    #[test]
    fn probe_failure_reopens_with_doubled_cooldown() {
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        let t1 = now + 301 * SEC;
        assert!(b.check(ip(), t1));
        b.on_failure(ip(), t1);
        // doubled to 600s: still open at +599, probe at +601
        assert!(!b.check(ip(), t1 + 599 * SEC));
        assert!(b.check(ip(), t1 + 601 * SEC));
    }

    #[test]
    fn cooldown_doubling_caps_at_900s() {
        let mut b = breaker(3, 300 * SEC);
        let mut now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        // fail probes repeatedly: 300 -> 600 -> 900 -> 900
        for _ in 0..3 {
            now += 1000 * SEC;
            assert!(b.check(ip(), now));
            b.on_failure(ip(), now);
        }
        assert!(!b.check(ip(), now + 899 * SEC));
        assert!(b.check(ip(), now + 901 * SEC));
    }

    #[test]
    fn packet_from_open_device_grants_early_probe() {
        // process_packet calls note_packet for every parseable packet,
        // e.g. a discovery-scan reply from a recovering device: the
        // cooldown is shortcut to "probe now", but only ONE probe.
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        assert!(!b.check(ip(), now));
        b.note_packet(ip(), now);
        assert!(b.check(ip(), now), "scan reply unlocks an immediate probe");
        assert!(!b.check(ip(), now), "but only one");
    }

    #[test]
    fn scan_reply_does_not_reset_failure_counter() {
        // Regression (squad P1): scans arrive every ~60s, polls every
        // 30s. If a scan reply cleared the counter, a device that
        // answers scans but drops status queries could never
        // accumulate `threshold` failures and the breaker would never
        // open for exactly the devices it targets.
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        b.on_failure(ip(), now);
        b.on_failure(ip(), now);
        b.note_packet(ip(), now);
        b.on_failure(ip(), now);
        assert!(!b.check(ip(), now), "third failure still opens");
    }

    #[test]
    fn scan_reply_during_half_open_probe_is_ignored() {
        // Regression (squad P1): a scan reply arriving while a probe
        // is in flight must not destroy the entry, or the probe's
        // failure would restart the ladder at base cooldown.
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        let t1 = now + 301 * SEC;
        assert!(b.check(ip(), t1), "probe granted");
        b.note_packet(ip(), t1);
        assert!(!b.check(ip(), t1), "probe slot still consumed");
        b.on_failure(ip(), t1);
        // ladder preserved: reopened with DOUBLED cooldown, not base
        assert!(!b.check(ip(), t1 + 599 * SEC));
        assert!(b.check(ip(), t1 + 601 * SEC));
    }

    #[test]
    fn probe_expiry_covers_large_query_schedules() {
        // Regression (CodeRabbit #40): with attempts=100 a probe
        // legitimately runs ~293s; a fixed 45s expiry would reclaim
        // the slot mid-probe and start a concurrent probe.
        let policy = LanQueryPolicy {
            attempts: 100,
            initial_backoff: 350 * MS,
        };
        let schedule_total: Duration = policy.schedule().iter().sum();
        let mut b = BreakerCore::with_policy(
            BreakerConfig {
                threshold: 3,
                base_cooldown: 300 * SEC,
            },
            &policy,
        );
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        let t1 = now + 301 * SEC;
        assert!(b.check(ip(), t1), "probe granted");
        // mid-schedule: the slot must NOT be reclaimed
        assert!(!b.check(ip(), t1 + schedule_total));
        // past schedule + margin: self-heal still works
        assert!(b.check(ip(), t1 + schedule_total + 16 * SEC));
    }

    #[test]
    fn cooldown_below_floor_is_clamped() {
        // cooldown ~0 would flip open->probe on every poll tick,
        // logging each time and suppressing nothing.
        let mut b = breaker(3, Duration::from_secs(1));
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        assert!(!b.check(ip(), now + 29 * SEC));
        assert!(b.check(ip(), now + 31 * SEC));
    }

    #[test]
    fn threshold_zero_disables_breaker() {
        let mut b = breaker(0, 300 * SEC);
        let now = Instant::now();
        for _ in 0..10 {
            b.on_failure(ip(), now);
        }
        assert!(b.check(ip(), now));
        assert!(b.by_ip.is_empty(), "disabled breaker tracks nothing");
    }

    #[test]
    fn stale_probe_self_heals() {
        // A probe whose task was cancelled must not wedge the breaker
        // in HalfOpen forever.
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        let t1 = now + 301 * SEC;
        assert!(b.check(ip(), t1), "probe granted, then lost");
        let t2 = t1 + HALF_OPEN_PROBE_EXPIRY + SEC;
        assert!(b.check(ip(), t2), "expired probe grants another");
    }

    #[test]
    fn ungated_failures_count_while_open() {
        // Confirm polls bypass check() but still record outcomes; a
        // failure while already open must not panic or reset state.
        let mut b = breaker(3, 300 * SEC);
        let now = Instant::now();
        for _ in 0..4 {
            b.on_failure(ip(), now);
        }
        assert!(!b.check(ip(), now));
        assert!(b.check(ip(), now + 301 * SEC));
    }

    #[test]
    fn distinct_ips_are_independent() {
        let mut b = breaker(3, 300 * SEC);
        let other: IpAddr = "192.168.1.51".parse().unwrap();
        let now = Instant::now();
        for _ in 0..3 {
            b.on_failure(ip(), now);
        }
        assert!(!b.check(ip(), now));
        assert!(b.check(other, now));
    }

    // -- Loopback integration: prove the on-wire retransmit bound --

    async fn loopback_client(policy: LanQueryPolicy) -> (Client, LanDevice, UdpSocket) {
        // Bind the device side first so client sends have a receiver.
        let device_sock = UdpSocket::bind(("127.0.0.1", CMD_PORT))
            .await
            .expect("bind 127.0.0.1:4003 for test device");
        let options = DiscoOptions {
            enable_multicast: false,
            additional_addresses: vec![],
            broadcast_all_interfaces: false,
            global_broadcast: false,
            query_policy: policy,
            breaker: BreakerConfig::default(),
        };
        let (client, _scan) = Client::new(options).await.expect("client");
        let device = LanDevice {
            ip: "127.0.0.1".parse().unwrap(),
            device: "AA:BB:CC:DD:EE:FF:00:11".to_string(),
            sku: "H6001".to_string(),
            ble_version_hard: String::new(),
            ble_version_soft: String::new(),
            wifi_version_hard: String::new(),
            wifi_version_soft: String::new(),
        };
        (client, device, device_sock)
    }

    /// Count datagrams arriving at the device socket until `idle`
    /// elapses with none. A real recv error is its own failure, not
    /// a low count — the assertion message must point at the cause.
    async fn count_datagrams(device_sock: &UdpSocket, idle: Duration) -> u32 {
        let mut buf = [0u8; 1024];
        let mut count = 0u32;
        loop {
            match tokio::time::timeout(idle, device_sock.recv_from(&mut buf)).await {
                Ok(Ok(_)) => count += 1,
                Ok(Err(err)) => panic!("recv_from failed: {err}"),
                Err(_) => break,
            }
        }
        count
    }

    fn breaker_failures(client: &Client, ip: IpAddr) -> Option<u32> {
        client
            .inner
            .breaker
            .lock()
            .unwrap()
            .by_ip
            .get(&ip)
            .map(|e| e.consecutive_failures)
    }

    // All loopback scenarios share fixed ports 4002/4003, so they run
    // inside ONE test to avoid a bind race under the parallel harness.
    // 100ms backoff gives the scenario-B reply a ~200ms window instead
    // of ~40ms — thin margins flake on loaded CI boxes.
    #[tokio::test]
    async fn loopback_retransmit_bound_and_reply() {
        let policy = LanQueryPolicy {
            attempts: 3,
            initial_backoff: 100 * MS,
        };
        let (client, device, device_sock) = loopback_client(policy).await;

        // Scenario A: device never answers. The client must send
        // exactly `attempts` datagrams and then fail. This is the
        // core claim of the change: 29 packets become 3.
        let query = client.query_status(&device);
        let counter = count_datagrams(&device_sock, 800 * MS);
        let (result, datagrams) = tokio::join!(query, counter);
        assert!(result.is_err(), "no reply must yield an error");
        assert_eq!(datagrams, 3, "exactly `attempts` datagrams on the wire");
        assert_eq!(
            breaker_failures(&client, device.ip),
            Some(1),
            "the failure must be recorded against the breaker"
        );

        // Scenario B: device answers the second attempt; the client
        // stops retransmitting after the reply.
        let query = client.query_status(&device);
        let responder = async {
            let mut buf = [0u8; 1024];
            let mut count = 0u32;
            loop {
                match tokio::time::timeout(800 * MS, device_sock.recv_from(&mut buf)).await {
                    Ok(Ok(_)) => {
                        count += 1;
                        if count == 2 {
                            let reply = serde_json::json!({
                                "msg": {
                                    "cmd": "devStatus",
                                    "data": {
                                        "onOff": 1,
                                        "brightness": 42,
                                        "color": {"r": 1, "g": 2, "b": 3},
                                        "colorTemInKelvin": 0,
                                    }
                                }
                            });
                            device_sock
                                .send_to(reply.to_string().as_bytes(), ("127.0.0.1", LISTEN_PORT))
                                .await
                                .expect("send reply");
                        }
                    }
                    Ok(Err(err)) => panic!("recv_from failed: {err}"),
                    Err(_) => break,
                }
            }
            count
        };
        let (result, datagrams) = tokio::join!(query, responder);
        let status = result.expect("reply must yield Ok");
        assert_eq!(status.brightness, 42);
        assert_eq!(datagrams, 2, "no retransmit after the reply");
        assert_eq!(
            breaker_failures(&client, device.ip),
            None,
            "success must clear the breaker entry"
        );

        // Scenario C: scenario B's success reset the counter, so
        // three fresh failed queries reach the threshold of 3; the
        // GATED path must then refuse without a single packet on
        // the wire. This is the breaker's load-bearing claim.
        for _ in 0..3 {
            let query = client.query_status(&device);
            let counter = count_datagrams(&device_sock, 800 * MS);
            let (result, _) = tokio::join!(query, counter);
            assert!(result.is_err());
        }
        let query = client.query_status_respecting_breaker(&device);
        let counter = count_datagrams(&device_sock, 500 * MS);
        let (result, datagrams) = tokio::join!(query, counter);
        assert!(result.is_err(), "open breaker must refuse");
        assert_eq!(datagrams, 0, "open breaker must send nothing");
    }
}
