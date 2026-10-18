use std::{env, ffi::OsString, fs, time::Duration};

use crate::SimulatorError;

/// Builds the simulator's HTTPS client for `origin`.
///
/// The development CA is deliberately scoped to loopback HTTPS destinations so it cannot broaden
/// trust for a production origin.
pub fn http_client(origin: &str) -> Result<reqwest::Client, SimulatorError> {
    let development_certificate = loopback_https(origin).then(|| env::var_os("PEPPY_DEV_TLS_CERT"));
    http_client_with_development_certificate(origin, development_certificate.flatten())
}

fn http_client_with_development_certificate(
    origin: &str,
    development_certificate: Option<OsString>,
) -> Result<reqwest::Client, SimulatorError> {
    http_client_builder(origin, development_certificate)?
        .build()
        .map_err(|_| SimulatorError::Http)
}

fn http_client_builder(
    origin: &str,
    development_certificate: Option<OsString>,
) -> Result<reqwest::ClientBuilder, SimulatorError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10));
    if loopback_https(origin)
        && let Some(path) = development_certificate
    {
        let pem = fs::read(path).map_err(|_| SimulatorError::DevelopmentCertificate)?;
        let certificates = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|_| SimulatorError::DevelopmentCertificate)?;
        if certificates.is_empty() {
            return Err(SimulatorError::DevelopmentCertificate);
        }
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    Ok(builder)
}

fn loopback_https(origin: &str) -> bool {
    let Ok(origin) = url::Url::parse(origin) else {
        return false;
    };
    if origin.scheme() != "https" {
        return false;
    }
    match origin.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::{SocketAddr, TcpListener, TcpStream},
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        thread,
        time::Duration,
    };

    use super::*;

    struct TlsServer(Child);

    impl Drop for TlsServer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn openssl(args: &[&str], directory: &Path) {
        assert!(
            Command::new("openssl")
                .args(args)
                .current_dir(directory)
                .status()
                .unwrap()
                .success()
        );
    }

    fn certificate_authority(directory: &Path, name: &str) -> PathBuf {
        let key = format!("{name}.key");
        let certificate = format!("{name}.pem");
        let subject = format!("/CN={name}");
        openssl(
            &[
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                &key,
                "-out",
                &certificate,
                "-subj",
                &subject,
                "-days",
                "1",
            ],
            directory,
        );
        directory.join(certificate)
    }

    fn tls_server(directory: &Path, ca: &Path) -> (TlsServer, u16) {
        openssl(
            &[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "server.key",
                "-out",
                "server.csr",
                "-subj",
                "/CN=localhost",
            ],
            directory,
        );
        fs::write(
            directory.join("extensions.cnf"),
            "subjectAltName=DNS:localhost\n",
        )
        .unwrap();
        openssl(
            &[
                "x509",
                "-req",
                "-in",
                "server.csr",
                "-CA",
                ca.to_str().unwrap(),
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                "server.pem",
                "-days",
                "1",
                "-extfile",
                "extensions.cnf",
            ],
            directory,
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let address = format!("127.0.0.1:{port}");
        let accept = openssl_accept_argument(&address, port);
        let mut child = Command::new("openssl")
            .args([
                "s_server",
                "-accept",
                &accept,
                "-cert",
                "server.pem",
                "-key",
                "server.key",
                "-www",
                "-quiet",
            ])
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_for_server(&mut child, &address);
        (TlsServer(child), port)
    }

    fn openssl_accept_argument(address: &str, port: u16) -> String {
        let version = Command::new("openssl").arg("version").output().unwrap();
        if String::from_utf8_lossy(&version.stdout).contains("LibreSSL") {
            // LibreSSL only accepts a port; IPv4 readiness is verified below.
            port.to_string()
        } else {
            address.into()
        }
    }

    fn wait_for_server(child: &mut Child, address: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if TcpStream::connect(address).is_ok() {
                return;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("OpenSSL TLS test server exited before readiness: {status}");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "OpenSSL TLS test server did not become ready"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn localhost_client(certificate: Option<std::ffi::OsString>, port: u16) -> reqwest::Client {
        http_client_builder("https://localhost", certificate)
            .unwrap()
            .resolve("localhost", SocketAddr::from(([127, 0, 0, 1], port)))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn development_ca_is_opt_in_and_keeps_tls_validation_enabled() {
        let directory = tempfile::TempDir::new().unwrap();
        let ca = certificate_authority(directory.path(), "ca");
        let wrong_ca = certificate_authority(directory.path(), "wrong-ca");
        let (server, port) = tls_server(directory.path(), &ca);
        let url = format!("https://localhost:{port}");

        let trusted = localhost_client(Some(ca.clone().into_os_string()), port);
        assert!(trusted.get(&url).send().await.is_ok());

        let no_ca = localhost_client(None, port)
            .get(&url)
            .send()
            .await
            .unwrap_err();
        assert!(format!("{no_ca:?}").contains("UnknownIssuer"));

        let wrong_ca = localhost_client(Some(wrong_ca.into_os_string()), port)
            .get(&url)
            .send()
            .await
            .unwrap_err();
        assert!(format!("{wrong_ca:?}").contains("UnknownIssuer"));

        let hostname_mismatch = localhost_client(Some(ca.into_os_string()), port)
            .get(format!("https://127.0.0.1:{port}"))
            .send()
            .await
            .unwrap_err();
        assert!(format!("{hostname_mismatch:?}").contains("NotValidForName"));

        drop(server);
    }

    #[test]
    fn malformed_development_ca_path_fails_closed() {
        assert!(matches!(
            http_client_with_development_certificate(
                "https://localhost",
                Some("/missing/peppy-development-ca.pem".into())
            ),
            Err(SimulatorError::DevelopmentCertificate)
        ));
    }

    #[test]
    fn empty_development_ca_fails_closed() {
        let directory = tempfile::TempDir::new().unwrap();
        let empty_certificate = directory.path().join("empty-ca.pem");
        fs::write(&empty_certificate, []).unwrap();
        assert!(matches!(
            http_client_with_development_certificate(
                "https://localhost",
                Some(empty_certificate.into_os_string())
            ),
            Err(SimulatorError::DevelopmentCertificate)
        ));
    }

    #[test]
    fn nonempty_invalid_development_ca_fails_closed() {
        let directory = tempfile::TempDir::new().unwrap();
        for (name, contents) in [
            ("garbage.pem", b"not a certificate".as_slice()),
            ("whitespace.pem", b" \n\t".as_slice()),
            (
                "malformed.pem",
                b"-----BEGIN CERTIFICATE-----\ninvalid\n".as_slice(),
            ),
        ] {
            let invalid_certificate = directory.path().join(name);
            fs::write(&invalid_certificate, contents).unwrap();
            assert!(matches!(
                http_client_with_development_certificate(
                    "https://localhost",
                    Some(invalid_certificate.into_os_string())
                ),
                Err(SimulatorError::DevelopmentCertificate)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_development_ca_path_fails_closed() {
        use std::os::unix::ffi::OsStringExt;

        assert!(matches!(
            http_client_with_development_certificate(
                "https://localhost",
                Some(std::ffi::OsString::from_vec(vec![
                    b'/', b'm', b'i', b's', b's', b'i', b'n', b'g', b'/', 0xFF,
                ]))
            ),
            Err(SimulatorError::DevelopmentCertificate)
        ));
    }

    #[test]
    fn absent_development_ca_uses_normal_trust() {
        assert!(http_client_with_development_certificate("https://localhost", None).is_ok());
    }

    #[test]
    fn development_ca_is_ignored_for_non_loopback_origins() {
        assert!(
            http_client_with_development_certificate(
                "https://api.example.test",
                Some("/missing/peppy-development-ca.pem".into())
            )
            .is_ok()
        );
    }
}
