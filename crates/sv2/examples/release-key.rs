//! The release signing tool (see `tools/release-firmware.sh`).
//!
//!   release-key keygen <keyfile>            new key; prints the public key
//!   release-key pubkey <keyfile>            the public key of a key file
//!   release-key sign   <keyfile> <image>    signature as 128 hex chars on stdout
//!   release-key verify <pubhex> <image> <sigfile>
//!
//! The key file holds the private key as 64 hex characters; it is
//! created with mode 600 and never printed.

use std::fs;
use std::io::Read;
use warpaxe_sv2::release::{digest, from_hex, keypair, public_key, sign, to_hex, verify};

fn read_key(path: &str) -> [u8; 32] {
    let s = fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    from_hex::<32>(&s).unwrap_or_else(|| die("key file is not 64 hex characters"))
}

fn read_image(path: &str) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")))
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("keygen") if a.len() == 3 => {
            if fs::metadata(&a[2]).is_ok() {
                die("key file exists, not overwriting");
            }
            let mut secret = [0u8; 32];
            fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut secret))
                .unwrap_or_else(|e| die(&format!("/dev/urandom: {e}")));
            let (sk, pk) = keypair(&secret).unwrap_or_else(|_| die("unlucky bytes, run again"));
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                use std::io::Write;
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&a[2])
                    .unwrap_or_else(|e| die(&format!("{}: {e}", a[2])));
                writeln!(f, "{}", to_hex(&sk)).unwrap();
            }
            #[cfg(not(unix))]
            fs::write(&a[2], format!("{}\n", to_hex(&sk))).unwrap();
            println!("{}", to_hex(&pk));
        }
        Some("pubkey") if a.len() == 3 => {
            let pk = public_key(&read_key(&a[2])).unwrap_or_else(|_| die("bad key"));
            println!("{}", to_hex(&pk));
        }
        Some("sign") if a.len() == 4 => {
            let sk = read_key(&a[2]);
            let d = digest(&read_image(&a[3]));
            let sig = sign(&sk, &d).unwrap_or_else(|_| die("bad key"));
            println!("{}", to_hex(&sig));
        }
        Some("verify") if a.len() == 5 => {
            let pk = from_hex::<32>(&a[2]).unwrap_or_else(|| die("public key: 64 hex characters"));
            let d = digest(&read_image(&a[3]));
            let s = fs::read_to_string(&a[4]).unwrap_or_else(|e| die(&format!("{}: {e}", a[4])));
            let sig = from_hex::<64>(&s).unwrap_or_else(|| die("signature: 128 hex characters"));
            if verify(&pk, &d, &sig) {
                println!("ok: sha256 {}", to_hex(&d));
            } else {
                die("signature does NOT verify");
            }
        }
        _ => die("usage: release-key keygen <keyfile> | pubkey <keyfile> | sign <keyfile> <image> | verify <pubhex> <image> <sigfile>"),
    }
}
