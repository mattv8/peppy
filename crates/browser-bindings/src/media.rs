use super::*;
use peppy_desktop_api::images::{
    MAX_IMAGE_SOURCE_BYTES, SharedImageError, is_previewable, preview_data_url, public_name,
    reencode_public,
};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

const COPY_BUFFER_BYTES: usize = 64 * 1024;

pub(super) fn handles(command: &str) -> bool {
    matches!(
        command,
        "attachment_info"
            | "prepare_attachment"
            | "pending_uploads"
            | "pending_downloads"
            | "_worker_attachment_remote"
            | "mark_attachment_uploaded"
            | "discard_unreferenced_attachment"
            | "install_downloaded_attachment"
            | "cipher_file"
            | "export_attachment"
            | "preview_attachment"
            | "public_image"
            | "prepare_contact_photo"
            | "contact_photo_transfer_state"
            | "acknowledge_contact_photo_reference"
            | "acknowledge_contact_photo_reclaim"
    )
}

impl BrowserCore {
    pub(super) fn media_command(&self, command: &str, args: Value) -> Result<Value, Failure> {
        match command {
            "attachment_info" => self.attachment_info(args),
            "prepare_attachment" => self.prepare_attachment(args),
            "pending_uploads" => self.pending_uploads(),
            "pending_downloads" => self.pending_downloads(),
            "_worker_attachment_remote" => self.attachment_remote(args),
            "mark_attachment_uploaded" => self.mark_attachment_uploaded(args),
            "discard_unreferenced_attachment" => self.discard_attachment(args),
            "install_downloaded_attachment" => self.install_downloaded_attachment(args),
            "cipher_file" => self.cipher_file(args),
            "export_attachment" => self.export_attachment(args),
            "preview_attachment" => self.preview_attachment(args),
            "public_image" => self.public_image(args),
            "prepare_contact_photo" => self.prepare_contact_photo(args),
            "contact_photo_transfer_state" => self.contact_photo_transfer_state(),
            "acknowledge_contact_photo_reference" => self.contact_photo_acknowledgement(args, true),
            "acknowledge_contact_photo_reclaim" => self.contact_photo_acknowledgement(args, false),
            _ => Err(unknown_command()),
        }
    }

    fn attachment_info(&self, args: Value) -> Result<Value, Failure> {
        let id = attachment_id(&args)?;
        serde_json::to_value(dto::attachment_view(
            &self.client()?.attachment_info(id).map_err(core)?,
            None,
            None,
        ))
        .map_err(|_| core(CoreError::Database))
    }
    fn prepare_attachment(&self, args: Value) -> Result<Value, Failure> {
        let name = args
            .get("temporaryInputFilename")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let media = args
            .get("mediaType")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let display = args
            .get("displayName")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let source = self.worker_input_file(name)?;
        let info = self
            .client()?
            .prepare_attachment(&source, media, display)
            .map_err(core)?;
        serde_json::to_value(dto::attachment_view(&info, None, None))
            .map_err(|_| core(CoreError::Database))
    }
    fn pending_uploads(&self) -> Result<Value, Failure> {
        Ok(json!(self.client()?.pending_uploads().map_err(core)?.into_iter().map(|item| json!({"id": item.attachment_id.to_string(), "ciphertextBytes": item.ciphertext_bytes})).collect::<Vec<_>>()))
    }
    fn pending_downloads(&self) -> Result<Value, Failure> {
        let downloads = self
            .client()?
            .pending_downloads()
            .map_err(core)?
            .into_iter()
            .map(|item| {
                let remote_object_id = item
                    .remote_object_id
                    .ok_or_else(|| core(CoreError::InvalidMedia))?;
                Ok(json!({
                    "id": item.attachment_id.to_string(),
                    "ciphertextBytes": item.ciphertext_bytes,
                    "remoteObjectId": remote_object_id,
                }))
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        Ok(json!(downloads))
    }
    /// Returns an opaque remote ID only to the Worker publication path, never a display DTO.
    fn attachment_remote(&self, args: Value) -> Result<Value, Failure> {
        let remote_object_id = self
            .client()?
            .attachment_remote_object_id(attachment_id(&args)?)
            .map_err(core)?;
        Ok(json!({"remoteObjectId": remote_object_id}))
    }
    fn mark_attachment_uploaded(&self, args: Value) -> Result<Value, Failure> {
        self.client()?
            .mark_attachment_uploaded(
                attachment_id(&args)?,
                args.get("remoteObjectId")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?,
            )
            .map_err(core)?;
        Ok(json!({}))
    }
    fn discard_attachment(&self, args: Value) -> Result<Value, Failure> {
        Ok(
            json!({"discarded": self.client()?.discard_unreferenced_attachment(attachment_id(&args)?).map_err(core)?}),
        )
    }

    fn install_downloaded_attachment(&self, args: Value) -> Result<Value, Failure> {
        let source = args
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let source = self.worker_input_file(source)?;
        self.client()?
            .install_downloaded_attachment(attachment_id(&args)?, &source)
            .map_err(core)?;
        fs::remove_file(source).map_err(|_| local_file_failure())?;
        Ok(json!({}))
    }

    /// Returns only a worker-relative encrypted object reference for the Worker transport.
    fn cipher_file(&self, args: Value) -> Result<Value, Failure> {
        let id = attachment_id(&args)?;
        let client = self.client()?;
        let info = client.attachment_info(id).map_err(core)?;
        let path = client.native_cipher_file(id).map_err(core)?;
        let length = fs::metadata(path).map_err(|_| local_file_failure())?.len();
        if length != info.ciphertext_bytes {
            return Err(core(CoreError::InvalidMedia));
        }
        Ok(json!({
            "file": format!("client.db.media/cipher/{id}.ppss"),
            "ciphertextBytes": length,
            "ciphertextSha256": info.ciphertext_sha256,
        }))
    }

    /// Copies an RAII-managed plaintext file into the Worker-only output hand-off directory.
    fn export_attachment(&self, args: Value) -> Result<Value, Failure> {
        let id = attachment_id(&args)?;
        let output_id = args
            .get("outputId")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(invalid)?;
        let client = self.client()?;
        let info = client.attachment_info(id).map_err(core)?;
        let output_dir = self.root.join("worker-output");
        fs::create_dir_all(&output_dir).map_err(|_| local_file_failure())?;
        let output = output_dir.join(output_id.to_string());
        let plaintext = client.open_native_plaintext(id).map_err(core)?;
        let copied = copy_exclusive(plaintext.path(), &output, info.plaintext_bytes)
            .map_err(|_| local_file_failure())?;
        Ok(json!({
            "file": format!("worker-output/{output_id}"),
            "length": copied,
            "type": info.media_type,
            "displayName": info.display_name,
        }))
    }

    /// Returns a bounded, freshly encoded PNG preview. The original plaintext never crosses RPC.
    fn preview_attachment(&self, args: Value) -> Result<Value, Failure> {
        let id = attachment_id(&args)?;
        let client = self.client()?;
        let info = client.attachment_info(id).map_err(core)?;
        if !info.state.is_local() || !is_previewable(&info.media_type) {
            return Ok(json!({}));
        }
        let plaintext = client.open_native_plaintext(id).map_err(core)?;
        let preview_url = read_capped(plaintext.path()).and_then(|bytes| preview_data_url(&bytes));
        Ok(json!({"previewUrl": preview_url}))
    }

    /// Writes a metadata-free derivative to a private Worker hand-off file. Publication and any
    /// public URL remain a future, explicitly confirmed host operation.
    fn public_image(&self, args: Value) -> Result<Value, Failure> {
        let id = attachment_id(&args)?;
        let output_id = args
            .get("outputId")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(invalid)?;
        let client = self.client()?;
        let info = client.attachment_info(id).map_err(core)?;
        if !info.state.is_local() || !is_previewable(&info.media_type) {
            return Err(image_failure(SharedImageError::Unsupported));
        }
        let plaintext = client.open_native_plaintext(id).map_err(core)?;
        let bytes = read_capped(plaintext.path())
            .ok_or_else(|| image_failure(SharedImageError::TooLarge))?;
        let derivative = reencode_public(&bytes).map_err(image_failure)?;
        let output_dir = self.root.join("worker-output");
        fs::create_dir_all(&output_dir).map_err(|_| local_file_failure())?;
        let output = output_dir.join(output_id.to_string());
        copy_exclusive_bytes(&derivative.bytes, &output).map_err(|_| local_file_failure())?;
        Ok(json!({
            "file": format!("worker-output/{output_id}"),
            "name": public_name(&info.display_name, derivative.extension),
            "contentType": if derivative.extension == "jpg" { "image/jpeg" } else { "image/png" },
            "byteSize": derivative.bytes.len(),
            "width": derivative.width,
            "height": derivative.height,
        }))
    }

    fn prepare_contact_photo(&self, args: Value) -> Result<Value, Failure> {
        let source = args
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let info = self
            .client()?
            .prepare_contact_photo(&self.worker_input_file(source)?)
            .map_err(core)?;
        serde_json::to_value(dto::attachment_view(&info, None, None))
            .map_err(|_| core(CoreError::Database))
    }

    fn contact_photo_transfer_state(&self) -> Result<Value, Failure> {
        json_result(
            self.client()?
                .contact_photo_transfer_state_json()
                .map_err(core)?,
        )
    }

    fn contact_photo_acknowledgement(
        &self,
        args: Value,
        reference: bool,
    ) -> Result<Value, Failure> {
        let result = if reference {
            self.client()?
                .acknowledge_contact_photo_reference_json(&args.to_string())
        } else {
            self.client()?
                .acknowledge_contact_photo_reclaim_json(&args.to_string())
        }
        .map_err(core)?;
        json_result(result)
    }

    fn worker_input_file(&self, source: &str) -> Result<PathBuf, Failure> {
        let relative = Path::new(source);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(invalid());
        }
        let mut path = self.root.join("worker-input");
        for component in relative.components() {
            let Component::Normal(component) = component else {
                return Err(invalid());
            };
            path.push(component);
            let metadata = fs::symlink_metadata(&path).map_err(|_| local_file_failure())?;
            if metadata.file_type().is_symlink() {
                return Err(invalid());
            }
        }
        if !fs::symlink_metadata(&path)
            .map_err(|_| local_file_failure())?
            .file_type()
            .is_file()
        {
            return Err(local_file_failure());
        }
        self.safe_worker_file(source)?;
        Ok(path)
    }
}

fn copy_exclusive(source: &Path, output_path: &Path, expected_bytes: u64) -> io::Result<u64> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(output_path)?;
    let result = (|| {
        let mut input = fs::File::open(source)?;
        let mut remaining = expected_bytes;
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        while remaining > 0 {
            let read =
                input.read(&mut buffer[..remaining.min(COPY_BUFFER_BYTES as u64) as usize])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "plaintext truncated",
                ));
            }
            std::io::Write::write_all(&mut output, &buffer[..read])?;
            remaining -= read as u64;
        }
        if input.read(&mut buffer[..1])? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "plaintext exceeds metadata",
            ));
        }
        output.sync_all()?;
        Ok(expected_bytes)
    })();
    if result.is_err() {
        drop(output);
        let _ = fs::remove_file(output_path);
    }
    result
}

fn copy_exclusive_bytes(bytes: &[u8], output_path: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(output_path)?;
    let result = std::io::Write::write_all(&mut output, bytes).and_then(|_| output.sync_all());
    if result.is_err() {
        drop(output);
        let _ = fs::remove_file(output_path);
    }
    result
}

fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_IMAGE_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_IMAGE_SOURCE_BYTES).then_some(bytes)
}

fn image_failure(error: SharedImageError) -> Failure {
    match error {
        SharedImageError::Unsupported => Failure::new(
            "public-copy-unsupported",
            "Only PNG, JPEG, WebP or GIF images can be shared as a public copy.",
        ),
        SharedImageError::TooLarge => Failure::new(
            "public-copy-too-large",
            "The image is too large for a public copy even after downscaling.",
        ),
    }
}

fn json_result(value: String) -> Result<Value, Failure> {
    serde_json::from_str(&value).map_err(|_| core(CoreError::Database))
}

fn local_file_failure() -> Failure {
    Failure::new("attachment-local", "The selected file could not be read.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use image::{DynamicImage, ImageFormat};
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    fn sample_png() -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([10, 20, 30, 255]),
        ));
        let mut bytes = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn response(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        let response: Value = serde_json::from_str(
            &core.dispatch(&json!({"command": command, "args": args}).to_string()),
        )
        .unwrap();
        assert_eq!(response["ok"], true, "{response}");
        response["value"].clone()
    }

    fn unlocked_core(root: &Path) -> BrowserCore {
        let vault = VaultId::new();
        let device = DeviceId::new();
        let mut core = BrowserCore::new(root.to_path_buf());
        response(
            &mut core,
            "open",
            json!({
                "vaultId": vault,
                "deviceId": device,
                "databaseKey": vec![7; 32],
                "origin": "https://peppy.test/",
                "deviceRole": "device",
            }),
        );
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        response(
            &mut core,
            "unlock",
            json!({"profile": profile, "header": header, "passphrase": "passphrase"}),
        );
        core
    }

    #[test]
    fn prepared_media_has_worker_cipher_metadata_and_a_transient_export() {
        let root = tempfile::tempdir().unwrap();
        let input_dir = root.path().join("worker-input");
        fs::create_dir(&input_dir).unwrap();
        let original = b"real attachment bytes";
        fs::write(input_dir.join("report.txt"), original).unwrap();
        let mut core = unlocked_core(root.path());

        let prepared = response(
            &mut core,
            "prepare_attachment",
            json!({
                "temporaryInputFilename": "report.txt",
                "mediaType": "text/plain",
                "displayName": "report.txt",
            }),
        );
        let id = prepared["id"].as_str().unwrap();
        assert!(prepared.get("remoteObjectId").is_none());
        assert_eq!(
            response(&mut core, "_worker_attachment_remote", json!({"id": id}))["remoteObjectId"],
            Value::Null
        );
        let cipher = response(&mut core, "cipher_file", json!({"id": id}));
        assert_eq!(cipher["file"], format!("client.db.media/cipher/{id}.ppss"));
        assert!(cipher["ciphertextBytes"].as_u64().unwrap() > original.len() as u64);
        assert!(cipher.get("key").is_none());

        let remote_object_id = Uuid::new_v4().to_string();
        response(
            &mut core,
            "mark_attachment_uploaded",
            json!({"id": id, "remoteObjectId": remote_object_id}),
        );
        assert_eq!(
            response(&mut core, "_worker_attachment_remote", json!({"id": id}))["remoteObjectId"],
            remote_object_id
        );

        let output_id = Uuid::new_v4();
        let exported = response(
            &mut core,
            "export_attachment",
            json!({"id": id, "outputId": output_id}),
        );
        assert_eq!(exported["file"], format!("worker-output/{output_id}"));
        assert_eq!(exported["length"], original.len() as u64);
        assert_eq!(exported["type"], "text/plain");
        assert_eq!(
            fs::read(
                root.path()
                    .join("worker-output")
                    .join(output_id.to_string())
            )
            .unwrap(),
            original
        );
        let repeated: Value = serde_json::from_str(
            &core.dispatch(
                &json!({"command": "export_attachment", "args": {"id": id, "outputId": output_id}})
                    .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(repeated["error"]["code"], "attachment-local");
        assert_eq!(
            fs::read(
                root.path()
                    .join("worker-output")
                    .join(output_id.to_string())
            )
            .unwrap(),
            original
        );
        assert!(
            fs::read_dir(root.path().join("client.db.media/plain"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn worker_input_rejects_traversal_and_symlink_sources() {
        let root = tempfile::tempdir().unwrap();
        let input_dir = root.path().join("worker-input");
        fs::create_dir(&input_dir).unwrap();
        fs::write(root.path().join("outside.txt"), b"outside").unwrap();
        let mut core = unlocked_core(root.path());

        let traversal: Value = serde_json::from_str(&core.dispatch(
            &json!({
                "command": "prepare_attachment",
                "args": {"temporaryInputFilename": "../outside.txt", "mediaType": "text/plain", "displayName": "x"}
            })
            .to_string(),
        ))
        .unwrap();
        assert_eq!(traversal["error"]["code"], "invalid-request");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join("outside.txt"), input_dir.join("link.txt"))
                .unwrap();
            let symlink: Value = serde_json::from_str(&core.dispatch(
                &json!({
                    "command": "prepare_attachment",
                    "args": {"temporaryInputFilename": "link.txt", "mediaType": "text/plain", "displayName": "x"}
                })
                .to_string(),
            ))
            .unwrap();
            assert_eq!(symlink["error"]["code"], "invalid-request");
        }
    }

    #[test]
    fn preview_attachment_reencodes_sqlcipher_plaintext_and_refuses_svg() {
        let root = tempfile::tempdir().unwrap();
        let input_dir = root.path().join("worker-input");
        fs::create_dir(&input_dir).unwrap();
        fs::write(input_dir.join("image.png"), sample_png()).unwrap();
        fs::write(
            input_dir.join("image.svg"),
            b"<svg><script>alert(1)</script></svg>",
        )
        .unwrap();
        let mut core = unlocked_core(root.path());
        let png = response(
            &mut core,
            "prepare_attachment",
            json!({
                "temporaryInputFilename": "image.png", "mediaType": "image/png", "displayName": "image.png"
            }),
        );
        let preview = response(&mut core, "preview_attachment", json!({"id": png["id"]}));
        let encoded = preview["previewUrl"]
            .as_str()
            .unwrap()
            .strip_prefix("data:image/png;base64,")
            .unwrap();
        assert_eq!(
            image::guess_format(&STANDARD.decode(encoded).unwrap()).unwrap(),
            ImageFormat::Png
        );

        let svg = response(
            &mut core,
            "prepare_attachment",
            json!({
                "temporaryInputFilename": "image.svg", "mediaType": "image/svg+xml", "displayName": "image.svg"
            }),
        );
        assert!(
            response(&mut core, "preview_attachment", json!({"id": svg["id"]}))
                .get("previewUrl")
                .is_none()
        );
    }

    #[test]
    fn public_image_writes_only_a_private_metadata_free_derivative() {
        let root = tempfile::tempdir().unwrap();
        let input_dir = root.path().join("worker-input");
        fs::create_dir(&input_dir).unwrap();
        let original = sample_png();
        fs::write(input_dir.join("image.png"), &original).unwrap();
        let mut core = unlocked_core(root.path());
        let prepared = response(
            &mut core,
            "prepare_attachment",
            json!({
                "temporaryInputFilename": "image.png", "mediaType": "image/png", "displayName": "../private image.png"
            }),
        );
        let output_id = Uuid::new_v4();
        let derivative = response(
            &mut core,
            "public_image",
            json!({"id": prepared["id"], "outputId": output_id}),
        );
        assert_eq!(derivative["file"], format!("worker-output/{output_id}"));
        assert_eq!(derivative["name"], "privateimage.png");
        assert!(derivative.get("url").is_none());
        assert!(derivative.get("displayName").is_none());
        let output = root
            .path()
            .join("worker-output")
            .join(output_id.to_string());
        let bytes = fs::read(&output).unwrap();
        assert_eq!(derivative["byteSize"], bytes.len());
        assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Png);
        assert_eq!(fs::read(input_dir.join("image.png")).unwrap(), original);

        let repeated: Value = serde_json::from_str(&core.dispatch(
            &json!({"command": "public_image", "args": {"id": prepared["id"], "outputId": output_id}}).to_string(),
        )).unwrap();
        assert_eq!(repeated["error"]["code"], "attachment-local");
        assert_eq!(fs::read(output).unwrap(), bytes);
        assert!(
            fs::read_dir(root.path().join("client.db.media/plain"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}
