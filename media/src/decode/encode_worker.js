/* Play Exact WebP encode — OffscreenCanvas.convertToBlob off the main thread.
 * Main owns ScrubChunkStore; this worker only returns bytes.
 * Classic worker (no ESM) for broad mobile support.
 */
'use strict';

self.onmessage = async function (ev) {
  var msg = ev.data || {};
  var id = msg.id;
  var frameId = msg.frameId;
  var epoch = msg.epoch;
  var width = msg.width | 0;
  var height = msg.height | 0;
  var quality = typeof msg.quality === 'number' ? msg.quality : 0.5;
  var bitmap = msg.bitmap;

  function fail(err) {
    try {
      if (bitmap && typeof bitmap.close === 'function') bitmap.close();
    } catch (_) {}
    self.postMessage({
      id: id,
      frameId: frameId,
      epoch: epoch,
      error: String(err && err.message ? err.message : err),
    });
  }

  try {
    if (typeof OffscreenCanvas === 'undefined') {
      throw new Error('OffscreenCanvas unavailable');
    }
    if (!bitmap) {
      throw new Error('missing bitmap');
    }
    var canvas = new OffscreenCanvas(Math.max(1, width), Math.max(1, height));
    var ctx = canvas.getContext('2d');
    if (!ctx) {
      throw new Error('2d unavailable');
    }
    ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    if (typeof bitmap.close === 'function') {
      bitmap.close();
    }
    bitmap = null;

    var blob = await canvas.convertToBlob({
      type: 'image/webp',
      quality: quality,
    });
    var buf = await blob.arrayBuffer();
    self.postMessage(
      {
        id: id,
        frameId: frameId,
        epoch: epoch,
        replace: !!msg.replace,
        width: canvas.width,
        height: canvas.height,
        mime: blob.type || 'image/webp',
        bytes: buf,
      },
      [buf],
    );
  } catch (err) {
    fail(err);
  }
};
