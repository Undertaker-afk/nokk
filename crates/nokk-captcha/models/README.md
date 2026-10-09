# models

Versioning: one directory per model, `hcaptcha-vN/model.onnx`, plus a
`manifest.json` entry pointing at it.

- `schema`: manifest format version, bump on breaking changes.
- `family`: challenge family, currently `hcaptcha`.
- `active`: model name used when no override is given.
- Each model entry records `name`, relative `path`, `sha256` of the
  `.onnx` file, and the label/threshold source.

No weights are shipped in this crate. Downloads verify `sha256`
before use. ONNX inference needs the `vision-onnx` feature (`ort`).
