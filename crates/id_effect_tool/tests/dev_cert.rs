//! The `id-effect dev` certificate (ADR 0014 D8, C13): a localhost CA and a 13-day ECDSA P-256
//! certificate that WebTransport's `serverCertificateHashes` accepts, kept, rotated before it
//! expires, and refused by the kit's prod posture.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use id_effect_kit::posture::{self, NO_DEV_CERTIFICATE, Posture, ProbeStatus};
use id_effect_tool::dev::cert::{self, Action};
use sha2::{Digest, Sha256};
use x509_parser::extensions::GeneralName;
use x509_parser::pem::Pem;
use x509_parser::public_key::PublicKey;

const DAY: u64 = 24 * 3600;

/// The DER certificates of the PEM file `path`, in order.
fn ders(path: &Path) -> Vec<Vec<u8>> {
  let bytes = std::fs::read(path).unwrap();
  Pem::iter_from_buffer(&bytes)
    .map(|pem| pem.unwrap().contents)
    .collect()
}

#[test]
fn dev_cert_valid_for_webtransport_hashes() {
  let dir = tempfile::tempdir().unwrap();
  let issued = cert::ensure(dir.path(), SystemTime::now()).unwrap();
  assert_eq!(issued.action, Action::Created);
  let chain = ders(&issued.cert);
  assert_eq!(chain.len(), 2, "leaf, then the CA");
  let (_, leaf) = x509_parser::parse_x509_certificate(&chain[0]).unwrap();
  let (_, ca) = x509_parser::parse_x509_certificate(&chain[1]).unwrap();

  // serverCertificateHashes: X.509v3, ECDSA P-256, validity at most 14 days, in effect now.
  let validity = leaf.validity();
  let lifetime = validity.not_after.timestamp() - validity.not_before.timestamp();
  assert_eq!(lifetime, 13 * 86_400, "13 days");
  assert!(validity.is_valid());
  assert_eq!(leaf.version(), x509_parser::x509::X509Version::V3);
  match leaf.public_key().parsed().unwrap() {
    PublicKey::EC(point) => assert_eq!(point.key_size(), 256),
    other => panic!("not an EC key: {other:?}"),
  }
  assert_eq!(
    leaf.signature_algorithm.algorithm,
    x509_parser::oid_registry::OID_SIG_ECDSA_WITH_SHA256
  );
  // The hash the browser is given is the SHA-256 of the leaf's DER.
  let sha: [u8; 32] = Sha256::digest(&chain[0]).into();
  assert_eq!(sha, issued.sha256);
  assert_eq!(issued.sha256_hex().len(), 64);

  // localhost, 127.0.0.1 and ::1; a server certificate signed by the CA.
  let names = leaf.subject_alternative_name().unwrap().unwrap();
  let mut dns = Vec::new();
  let mut ips = Vec::new();
  for name in &names.value.general_names {
    match name {
      GeneralName::DNSName(n) => dns.push(n.to_string()),
      GeneralName::IPAddress(ip) => ips.push(ip.to_vec()),
      _ => {}
    }
  }
  assert_eq!(dns, ["localhost"]);
  assert!(ips.contains(&vec![127, 0, 0, 1]));
  assert!(ips.contains(&std::net::Ipv6Addr::LOCALHOST.octets().to_vec()));
  assert!(!leaf.is_ca());
  assert!(ca.is_ca());
  assert_eq!(leaf.issuer(), ca.subject());
  assert_eq!(ders(&issued.ca), vec![chain[1].clone()]);
  let ca_life = ca.validity().not_after.timestamp() - ca.validity().not_before.timestamp();
  assert_eq!(ca_life, 365 * 86_400);

  // Every certificate names the dev organization the kit's posture refuses in prod.
  for der in &chain {
    assert!(posture::is_dev_certificate(der));
  }
  assert_eq!(
    cert::DEV_CERTIFICATE_ORGANIZATION,
    posture::DEV_CERTIFICATE_ORGANIZATION
  );
  let organization: Vec<&str> = leaf
    .subject()
    .iter_organization()
    .map(|o| o.as_str().unwrap())
    .collect();
  assert_eq!(organization, [cert::DEV_CERTIFICATE_ORGANIZATION]);

  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    for key in [dir.path().join(cert::CA_KEY_FILE), issued.key.clone()] {
      let mode = std::fs::metadata(&key).unwrap().permissions().mode();
      assert_eq!(mode & 0o077, 0, "{} is private", key.display());
    }
  }
  let steps = issued.trust_instructions();
  assert!(steps.contains(&issued.ca.display().to_string()), "{steps}");
  assert!(steps.contains(&issued.sha256_hex()), "{steps}");
}

#[test]
fn dev_cert_is_kept_then_rotated_before_expiry() {
  let dir = tempfile::tempdir().unwrap();
  let now = SystemTime::now();
  let first = cert::ensure(dir.path(), now).unwrap();
  assert_eq!(first.action, Action::Created);
  let ca_pem = std::fs::read(&first.ca).unwrap();

  let again = cert::ensure(dir.path(), now + Duration::from_secs(DAY)).unwrap();
  assert_eq!(again.action, Action::Kept);
  assert_eq!(again.sha256, first.sha256);

  // Less than four days left: a new leaf from the same CA.
  let later = now + Duration::from_secs(10 * DAY);
  let renewed = cert::ensure(dir.path(), later).unwrap();
  assert_eq!(renewed.action, Action::Renewed);
  assert_ne!(renewed.sha256, first.sha256);
  assert!(renewed.not_after > first.not_after);
  assert_eq!(renewed.ca_not_after, first.ca_not_after);
  assert_eq!(std::fs::read(&renewed.ca).unwrap(), ca_pem);
  let chain = ders(&renewed.cert);
  let (_, leaf) = x509_parser::parse_x509_certificate(&chain[0]).unwrap();
  let ca_der = ders(&renewed.ca).remove(0);
  let (_, ca) = x509_parser::parse_x509_certificate(&ca_der).unwrap();
  // Issued by the stored CA: its name, and its key (the authority key identifier).
  assert_eq!(leaf.issuer(), ca.subject());
  let aki = leaf
    .extensions()
    .iter()
    .find_map(|e| match e.parsed_extension() {
      x509_parser::extensions::ParsedExtension::AuthorityKeyIdentifier(aki) => {
        aki.key_identifier.as_ref().map(|k| k.0.to_vec())
      }
      _ => None,
    })
    .unwrap();
  let ski = ca
    .extensions()
    .iter()
    .find_map(|e| match e.parsed_extension() {
      x509_parser::extensions::ParsedExtension::SubjectKeyIdentifier(ski) => Some(ski.0.to_vec()),
      _ => None,
    })
    .unwrap();
  assert_eq!(aki, ski);

  // A deleted leaf is reissued too.
  std::fs::remove_file(&renewed.key).unwrap();
  let reissued = cert::ensure(dir.path(), later).unwrap();
  assert_eq!(reissued.action, Action::Renewed);

  // The CA close to its end: a new CA (trust it again).
  let year = now + Duration::from_secs(340 * DAY);
  let replaced = cert::ensure(dir.path(), year).unwrap();
  assert_eq!(replaced.action, Action::Created);
  assert_ne!(std::fs::read(&replaced.ca).unwrap(), ca_pem);

  // Broken metadata starts over.
  std::fs::write(dir.path().join(cert::META_FILE), "{\"sha256\": \"zz\"}").unwrap();
  assert_eq!(
    cert::ensure(dir.path(), now).unwrap().action,
    Action::Created
  );
  // A CA key that does not parse is an error.
  cert::ensure(dir.path(), now).unwrap();
  std::fs::write(dir.path().join(cert::CA_KEY_FILE), "not a key").unwrap();
  let err = cert::ensure(dir.path(), now + Duration::from_secs(10 * DAY)).unwrap_err();
  assert!(err.contains(cert::CA_KEY_FILE), "{err}");
}

/// C13 end to end: the certificate `id-effect dev` issues fails the prod posture.
#[test]
fn dev_cert_refused_by_prod_posture() {
  let dir = tempfile::tempdir().unwrap();
  let issued = cert::ensure(dir.path(), SystemTime::now()).unwrap();
  let leaf = ders(&issued.cert).remove(0);
  let prod = Posture::new("prod").tls_certificate(&leaf).evaluate(&[]);
  assert_eq!(
    prod.probe(NO_DEV_CERTIFICATE).unwrap().status,
    ProbeStatus::Fail
  );
  assert!(prod.enforce().is_err());
  let dev = Posture::new("dev").tls_certificate(&leaf).evaluate(&[]);
  assert!(dev.passed(), "{dev}");
}

#[test]
fn dev_refuses_a_production_profile() {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
  let output = Command::new(env!("CARGO_BIN_EXE_id-effect"))
    .args(["dev", "--path"])
    .arg(dir.path())
    .env("ID_EFFECT_PROFILE", "prod")
    .env("ID_EFFECT_DEV_CERT_DIR", dir.path().join("certs"))
    .output()
    .unwrap();
  assert_eq!(output.status.code(), Some(1));
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(stderr.contains("dev profile only"), "{stderr}");
  assert!(!dir.path().join("certs").exists(), "no certificate issued");
}

/// `--cert-only` issues the certificate and prints the variables a script starts the app with.
#[test]
fn cert_only_prints_the_tls_variables() {
  let dir = tempfile::tempdir().unwrap();
  let certs = dir.path().join("certs");
  let run = |profile: Option<&str>| {
    let mut command = Command::new(env!("CARGO_BIN_EXE_id-effect"));
    command
      .args(["dev", "--cert-only", "--cert-dir"])
      .arg(&certs)
      .env_remove("ID_EFFECT_PROFILE");
    if let Some(profile) = profile {
      command.env("ID_EFFECT_PROFILE", profile);
    }
    command.output().unwrap()
  };
  let refused = run(Some("prod"));
  assert_eq!(refused.status.code(), Some(1));
  assert!(!certs.exists());

  let first = run(None);
  assert!(first.status.success(), "{first:?}");
  let stdout = String::from_utf8(first.stdout).unwrap();
  assert!(stdout.contains("trust the development CA once"), "{stdout}");
  let value = |text: &str, name: &str| {
    text
      .lines()
      .find_map(|l| l.strip_prefix(&format!("{name}=")))
      .map(str::to_owned)
      .unwrap_or_else(|| panic!("no {name} in {text}"))
  };
  assert_eq!(
    value(&stdout, "TLS_CERT_FILE"),
    certs.join(cert::CERT_FILE).display().to_string()
  );
  assert_eq!(
    value(&stdout, "TLS_KEY_FILE"),
    certs.join(cert::KEY_FILE).display().to_string()
  );
  assert_eq!(
    value(&stdout, "ID_EFFECT_DEV_CA_FILE"),
    certs.join(cert::CA_FILE).display().to_string()
  );
  let hash = value(&stdout, cert::SHA256_ENV_VAR);
  // A second run keeps the certificate.
  let second = String::from_utf8(run(Some("dev")).stdout).unwrap();
  assert_eq!(value(&second, cert::SHA256_ENV_VAR), hash);
  assert!(!second.contains("trust the development CA"), "{second}");
}

#[test]
fn default_dir_follows_the_environment() {
  use std::ffi::OsString;
  use std::path::PathBuf;

  let env = |pairs: &'static [(&'static str, &'static str)]| {
    move |name: &str| {
      pairs
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| OsString::from(v))
    }
  };
  assert_eq!(
    cert::default_dir_from(env(&[(cert::DIR_ENV_VAR, "/certs"), ("HOME", "/home/a")])),
    PathBuf::from("/certs")
  );
  assert_eq!(
    cert::default_dir_from(env(&[(cert::DIR_ENV_VAR, ""), ("XDG_DATA_HOME", "/data")])),
    PathBuf::from("/data/id-effect/dev-cert")
  );
  assert_eq!(
    cert::default_dir_from(env(&[("HOME", "/home/a")])),
    PathBuf::from("/home/a/.local/share/id-effect/dev-cert")
  );
  assert_eq!(
    cert::default_dir_from(env(&[])),
    std::env::temp_dir().join("id-effect/dev-cert")
  );
  let dir = cert::default_dir();
  assert!(dir.ends_with("id-effect/dev-cert") || std::env::var_os(cert::DIR_ENV_VAR).is_some());
}

/// A file that cannot be written is an error naming it.
#[test]
fn unwritable_files_are_errors() {
  for name in [
    cert::CA_FILE,
    cert::CA_KEY_FILE,
    cert::CERT_FILE,
    cert::KEY_FILE,
    cert::META_FILE,
  ] {
    let dir = tempfile::tempdir().unwrap();
    // A directory in the file's place.
    std::fs::create_dir(dir.path().join(name)).unwrap();
    let err = cert::ensure(dir.path(), SystemTime::now()).unwrap_err();
    assert!(err.contains(name), "{name}: {err}");
  }
}
