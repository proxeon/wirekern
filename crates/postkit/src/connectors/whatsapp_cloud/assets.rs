//! Phone-scoped media upload, metadata, download, and delete.
use crate::error::Error;
use crate::facets::WhatsAppAssets;
use crate::types::{AccountCreds, AppConfig, Deadline};
use crate::whatsapp::{
    validate_media_upload, WhatsAppMediaMeta, WhatsAppMediaUpload, WhatsAppUploadedMedia,
};
use async_trait::async_trait;
use serde_json::Value;

use super::graph::{access_token, map_graph_error, phone_number_id, read_json, value_string};
use super::WhatsAppCloud;

#[async_trait]
impl WhatsAppAssets for WhatsAppCloud {
    async fn upload_media(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        upload: &WhatsAppMediaUpload,
        deadline: Deadline,
    ) -> Result<WhatsAppUploadedMedia, Error> {
        validate_media_upload(upload).map_err(|reason| Error::InvalidPost {
            site: self.site.clone(),
            reason,
            limit: None,
        })?;
        let phone_number_id = phone_number_id(app)?;
        let token = access_token(creds)?;
        let part = reqwest::multipart::Part::bytes(upload.bytes.clone())
            .file_name(upload.filename.clone())
            .mime_str(&upload.mime_type)
            .map_err(|_| Error::InvalidPost {
                site: self.site.clone(),
                reason: "media_mime_unsupported".into(),
                limit: None,
            })?;
        let form = reqwest::multipart::Form::new()
            .text("messaging_product", "whatsapp")
            .text("type", upload.mime_type.clone())
            .part("file", part);
        let response = self
            .http
            .send(
                self.http
                    .post(&format!("{}/{phone_number_id}/media", self.base))
                    .bearer_auth(token)
                    .multipart(form),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        let id = body
            .get("id")
            .and_then(value_string)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_media_id".into(),
                message: "WhatsApp media upload returned no id".into(),
            })?;
        Ok(WhatsAppUploadedMedia { id })
    }

    async fn media_metadata(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<WhatsAppMediaMeta, Error> {
        if media_id.is_empty() {
            return Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "media_id_empty".into(),
                limit: None,
            });
        }
        let phone = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .get(&format!("{}/{media_id}?phone_number_id={phone}", self.base))
                    .bearer_auth(token),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        Ok(WhatsAppMediaMeta {
            id: body
                .get("id")
                .and_then(value_string)
                .unwrap_or_else(|| media_id.to_string()),
            mime_type: body.get("mime_type").and_then(value_string),
            sha256: body.get("sha256").and_then(value_string),
            file_size: body.get("file_size").and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            }),
            url: body.get("url").and_then(value_string),
        })
    }

    async fn download_media(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<Vec<u8>, Error> {
        let meta = self.media_metadata(app, creds, media_id, deadline).await?;
        let url = meta
            .url
            .filter(|u| media_download_url_allowed(u))
            .ok_or_else(|| Error::Platform {
                site: self.site.clone(),
                code: "missing_media_url".into(),
                message: "WhatsApp media metadata had no https url".into(),
            })?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(self.http.get(&url).bearer_auth(token), deadline, &self.site)
            .await?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let text = response.text().await.unwrap_or_default();
            return Err(map_graph_error(status, &text));
        }
        Ok(response
            .bytes()
            .await
            .map_err(|_| Error::request_failed(&self.site))?
            .to_vec())
    }

    async fn delete_media(
        &self,
        app: &AppConfig,
        creds: &AccountCreds,
        media_id: &str,
        deadline: Deadline,
    ) -> Result<(), Error> {
        if media_id.is_empty() {
            return Err(Error::InvalidPost {
                site: self.site.clone(),
                reason: "media_id_empty".into(),
                limit: None,
            });
        }
        let phone = phone_number_id(app)?;
        let token = access_token(creds)?;
        let response = self
            .http
            .send(
                self.http
                    .delete(&format!("{}/{media_id}?phone_number_id={phone}", self.base))
                    .bearer_auth(token),
                deadline,
                &self.site,
            )
            .await?;
        let body = read_json(response, &self.site).await?;
        if body.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Platform {
                site: self.site.clone(),
                code: "media_delete_failed".into(),
                message: "WhatsApp media delete did not return success".into(),
            });
        }
        Ok(())
    }
}

/// The exact limited JSON grammar Postkit allows on the message endpoint.
/// Keeping it public makes wire tests and embedding callers inspectable
/// without permitting arbitrary unreviewed JSON components.
pub(super) fn media_download_url_allowed(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://127.0.0.1:")
        || url.starts_with("http://localhost:")
}
