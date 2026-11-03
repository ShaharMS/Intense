Synthetic lossless 129 x 65 unsigned12bitRGB JPEG2000 fixtures. Both encode the
same generated pixel data; mct0 disables the multiple-component transform and
mct1 enables it. They exercise the actual OpenJPEG wrapper normalization path.
These are test inputs, not viewer samples or runtime warmup images.

SHA256:
- mct0.jp2:567ea75991ee4eb51efced6772eb015321be972c7cba4c54cdbd510efcccba02
- mct1.jp2:ca4a28da3a3177a4d675be61647bb35a7d69a2f03ff3af26a9c3648b6a6fdeec

Original diagnostic provenance is retained in
reports/kakadu-io-m303/pixels12-m561/provenance.json. No existing served image
was modified. The conversion comparison uses the wrapper's original RGB16
pixel path as its rounding reference.
