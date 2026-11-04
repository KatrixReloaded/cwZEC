//! Offline setup only: validates the destination and pins its authenticated envelope.
//! No RPC, signing, database access or broadcast. The caller supplies a reserved CREATE nonce.
use alloy::primitives::{Address, B256, Bytes, keccak256};
use anyhow::{Context, Result, ensure};
use app::core::destination::DestinationKey;
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    service: Address,
    destination: String,
}

#[derive(Serialize, Deserialize)]
struct Setup {
    chain_id: u64,
    token: Address,
    service: Address,
    deployer: Address,
    nonce: u64,
    balancer: Address,
    destination: String,
    envelope: Bytes,
    destination_hash: B256,
}

fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let local = root.join(".local");
    let intent: Intent = serde_json::from_reader(File::open(local.join("balancer-intent.json"))?)?;
    ensure!(intent.service != Address::ZERO, "service must not be zero");
    zcash::core::decode_address(&intent.destination).context("invalid recycling destination")?;
    let args: Vec<_> = env::args().skip(1).collect();
    if args.is_empty() {
        println!(
            "Recycling destination accepted by the bridge decoder; pass DEPLOYER NONCE to prepare."
        );
        return Ok(());
    }
    ensure!(args.len() == 2, "usage: prepare_balancer DEPLOYER NONCE");
    let deployer: Address = args[0].parse()?;
    ensure!(deployer != Address::ZERO, "deployer must not be zero");
    let nonce: u64 = args[1].parse()?;
    let balancer = deployer.create(nonce);
    let deployment: serde_json::Value =
        serde_json::from_reader(File::open(local.join("backed-deployment.json"))?)?;
    let token: Address = deployment["contract"]
        .as_str()
        .context("missing token")?
        .parse()?;
    let chain_id = 11155111;
    let key = DestinationKey::load(Path::new(&env::var("BRIDGE_DESTINATION_KEY_PATH")?))?;
    let path = local.join("balancer-setup.json");
    let setup = if path.try_exists()? {
        serde_json::from_reader::<_, Setup>(File::open(&path)?)?
    } else {
        let envelope = key.encrypt_destination(chain_id, token, balancer, &intent.destination)?;
        Setup {
            chain_id,
            token,
            service: intent.service,
            deployer,
            nonce,
            balancer,
            destination: intent.destination.clone(),
            destination_hash: keccak256(&envelope),
            envelope,
        }
    };
    ensure!(
        setup.chain_id == chain_id
            && setup.token == token
            && setup.service == intent.service
            && setup.deployer == deployer
            && setup.nonce == nonce
            && setup.balancer == balancer
            && setup.destination == intent.destination
            && setup.destination_hash == keccak256(&setup.envelope),
        "existing balancer setup conflicts with requested terms; preserve it and investigate"
    );
    ensure!(
        *key.decrypt_destination(chain_id, token, balancer, &setup.envelope)? == intent.destination,
        "authenticated destination differs from intended wallet"
    );
    if !path.try_exists()? {
        save_new(&path, &serde_json::to_vec_pretty(&setup)?)?;
    }
    let constructor = format!("{token} {} {}\n", setup.service, setup.destination_hash);
    let constructor_path = local.join("balancer-constructor-args.txt");
    if constructor_path.try_exists()? {
        ensure!(
            fs::read_to_string(&constructor_path)? == constructor,
            "constructor file conflicts with setup"
        );
    } else {
        save_new(&constructor_path, constructor.as_bytes())?;
    }
    File::open(&local)?.sync_all()?;
    println!(
        "Authenticated envelope saved privately.\nDeployer: {deployer}\nNonce: {nonce}\nPredicted balancer: {balancer}\nService: {}\nDestination hash: {}",
        setup.service, setup.destination_hash
    );
    Ok(())
}

fn save_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
