use std::{env, fs, process};

use base64::{engine::general_purpose::STANDARD, Engine};
use minisign_verify::{PublicKey, Signature};

fn decode_wrapped(value: &str, label: &str) -> Result<String, String> {
    let bytes = STANDARD
        .decode(value.trim())
        .map_err(|error| format!("invalid base64 in {label}: {error}"))?;
    String::from_utf8(bytes).map_err(|error| format!("invalid UTF-8 in {label}: {error}"))
}

fn verify() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let archive_path = args
        .next()
        .ok_or("usage: verify_update_signature <archive> <signature> <public-key>".to_string())?;
    let signature_path = args
        .next()
        .ok_or("missing updater signature path".to_string())?;
    let public_key_wrapped = args
        .next()
        .ok_or("missing updater public key".to_string())?;
    if args.next().is_some() {
        return Err("unexpected extra argument".to_string());
    }

    let archive = fs::read(&archive_path)
        .map_err(|error| format!("could not read updater archive {archive_path}: {error}"))?;
    let signature_wrapped = fs::read_to_string(&signature_path)
        .map_err(|error| format!("could not read updater signature {signature_path}: {error}"))?;

    let public_key = PublicKey::decode(&decode_wrapped(&public_key_wrapped, "updater public key")?)
        .map_err(|error| format!("invalid updater public key: {error}"))?;
    let signature = Signature::decode(&decode_wrapped(&signature_wrapped, "updater signature")?)
        .map_err(|error| format!("invalid updater signature: {error}"))?;

    public_key
        .verify(&archive, &signature, true)
        .map_err(|error| format!("updater signature verification failed: {error}"))?;
    println!("Updater signature is valid.");
    Ok(())
}

fn main() {
    if let Err(error) = verify() {
        eprintln!("{error}");
        process::exit(1);
    }
}
