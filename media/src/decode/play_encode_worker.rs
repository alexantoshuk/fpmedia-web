//! JS classic Worker for scrub / Play Exact WebP encode (`encode_worker.js`).
//!
//! Main: VF → `ImageBitmap` (blit LRU sync + transfer copy) → drop VF.
//! Worker: `convertToBlob('image/webp')` → main `put_chunk` / `insert_chunk`.
//!
//! Depth-1 in-flight + small pending queue (densify). Play skips when busy.
//! Epoch bump clears pending. Sync `toDataURL` fallback if Worker unavailable.

use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use web_sys::{Blob, BlobPropertyBag, ImageBitmap, MessageEvent, Url, Worker};

use crate::decode::scrub_encode::{SCRUB_WEBP_QUALITY, ScrubChunk};
use crate::error::MediaError;
use crate::types::FrameId;

const WORKER_SRC: &str = include_str!("encode_worker.js");

/// Create a classic Worker from the embedded script. Caller owns `onmessage` Closure.
pub fn spawn_play_webp_worker(
    onmessage: &Closure<dyn FnMut(MessageEvent)>,
) -> Result<(Worker, String), MediaError> {
    let parts = Array::new();
    parts.push(&JsValue::from_str(WORKER_SRC));
    let opts = BlobPropertyBag::new();
    opts.set_type("text/javascript");
    let blob = Blob::new_with_str_sequence_and_options(&parts, &opts).map_err(MediaError::from)?;
    let url = Url::create_object_url_with_blob(&blob).map_err(MediaError::from)?;
    let worker = Worker::new(&url).map_err(|e| {
        let _ = Url::revoke_object_url(&url);
        MediaError::from(e)
    })?;
    worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    Ok((worker, url))
}

/// Params for one Worker encode post.
pub struct PlayWebpJob<'a> {
    pub worker: &'a Worker,
    pub id: u64,
    pub frame: FrameId,
    pub epoch: u64,
    pub width: u32,
    pub height: u32,
    pub replace: bool,
    pub bitmap: ImageBitmap,
}

/// Post one encode job. Transfers `bitmap` (neutered on success).
pub fn post_play_webp_job(job: PlayWebpJob<'_>) -> Result<(), MediaError> {
    let PlayWebpJob {
        worker,
        id,
        frame,
        epoch,
        width,
        height,
        replace,
        bitmap,
    } = job;
    let msg = Object::new();
    Reflect::set(&msg, &"id".into(), &JsValue::from_f64(id as f64)).map_err(MediaError::from)?;
    Reflect::set(&msg, &"frameId".into(), &JsValue::from_f64(frame as f64))
        .map_err(MediaError::from)?;
    Reflect::set(&msg, &"epoch".into(), &JsValue::from_f64(epoch as f64))
        .map_err(MediaError::from)?;
    Reflect::set(&msg, &"width".into(), &JsValue::from_f64(width as f64))
        .map_err(MediaError::from)?;
    Reflect::set(&msg, &"height".into(), &JsValue::from_f64(height as f64))
        .map_err(MediaError::from)?;
    Reflect::set(
        &msg,
        &"quality".into(),
        &JsValue::from_f64(SCRUB_WEBP_QUALITY),
    )
    .map_err(MediaError::from)?;
    Reflect::set(&msg, &"replace".into(), &JsValue::from_bool(replace))
        .map_err(MediaError::from)?;
    Reflect::set(&msg, &"bitmap".into(), bitmap.as_ref()).map_err(MediaError::from)?;

    let transfer = Array::new();
    transfer.push(bitmap.as_ref());
    match worker.post_message_with_transfer(&msg, &transfer) {
        Ok(()) => {
            drop(bitmap);
            Ok(())
        }
        Err(e) => {
            bitmap.close();
            Err(MediaError::from(e))
        }
    }
}

/// Parse worker reply; `None` = error / malformed (caller clears busy).
pub struct PlayWebpReply {
    pub frame: FrameId,
    pub epoch: u64,
    pub replace: bool,
    pub chunk: ScrubChunk,
}

pub fn parse_play_webp_reply(ev: &MessageEvent) -> Result<Option<PlayWebpReply>, String> {
    let data = ev.data();
    if data.is_undefined() || data.is_null() {
        return Err("empty worker message".into());
    }
    let err = Reflect::get(&data, &"error".into()).ok();
    if let Some(err) = err
        && !err.is_undefined()
        && !err.is_null()
    {
        return Err(err.as_string().unwrap_or_else(|| format!("{err:?}")));
    }

    let frame = Reflect::get(&data, &"frameId".into())
        .ok()
        .and_then(|v| v.as_f64())
        .map(|f| f as FrameId)
        .ok_or_else(|| "missing frameId".to_string())?;
    let epoch = Reflect::get(&data, &"epoch".into())
        .ok()
        .and_then(|v| v.as_f64())
        .map(|f| f as u64)
        .unwrap_or(0);
    let replace = Reflect::get(&data, &"replace".into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let width = Reflect::get(&data, &"width".into())
        .ok()
        .and_then(|v| v.as_f64())
        .map(|f| f as u32)
        .unwrap_or(1)
        .max(1);
    let height = Reflect::get(&data, &"height".into())
        .ok()
        .and_then(|v| v.as_f64())
        .map(|f| f as u32)
        .unwrap_or(1)
        .max(1);
    let mime = Reflect::get(&data, &"mime".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| "image/webp".into());
    let bytes_val = Reflect::get(&data, &"bytes".into()).map_err(|e| format!("{e:?}"))?;
    let bytes = array_buffer_to_vec(&bytes_val).ok_or_else(|| "missing bytes".to_string())?;
    if bytes.is_empty() {
        return Err("empty webp".into());
    }
    Ok(Some(PlayWebpReply {
        frame,
        epoch,
        replace,
        chunk: ScrubChunk {
            bytes,
            width,
            height,
            mime,
        },
    }))
}

fn array_buffer_to_vec(val: &JsValue) -> Option<Vec<u8>> {
    if val.is_undefined() || val.is_null() {
        return None;
    }
    let u8 = Uint8Array::new(val);
    let mut bytes = vec![0u8; u8.length() as usize];
    u8.copy_to(&mut bytes);
    Some(bytes)
}

pub fn revoke_worker_url(url: &str) {
    let _ = Url::revoke_object_url(url);
}
