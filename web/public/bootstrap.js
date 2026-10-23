// Only the wasm-bindgen loader lives in JavaScript. The UI, connection state,
// protocol, cryptography and renderer are Rust.
import init from './pkg/horizon_web.js';
init().catch(() => {
  document.getElementById('status').textContent =
    'Could not load WebAssembly. Run web/build.ps1, then start the gateway.';
});
