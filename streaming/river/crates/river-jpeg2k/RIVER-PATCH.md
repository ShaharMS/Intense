Upstream jpeg2k 0.10.1, commit 74aaa2f8c733270ca90403064be81d93e2d81a8a, https://github.com/Neopallium/jpeg2k.

River adds only a per-DecodeParameters threads builder and applies its bounded budget after decoder setup. Default None preserves upstream behavior. threads(1) selects synchronous decoding. Pixel conversion, ROI selection, component handling and codec behavior are otherwise unchanged. LICENSE is preserved.
