# fox-4k.jpg

- Source: "Brown fox on snow field", Unsplash, https://unsplash.com/photos/brown-fox-on-snow-field-xUUZcpQlqpM
- License: Unsplash License (free to use, commercial included, no attribution required).
- Size: 3840 x 2556.

`../photo-splats.ply` was made from it:

```bash
swift tools/mask.swift pets/fox/reference/fox-4k.jpg /tmp/mask.png          # Vision foreground mask
sharp predict -i <dir with fox-4k.jpg> -o /tmp/out                           # Apple SHARP, single image -> 3DGS
python tools/crop_splats.py /tmp/out/fox-4k.ply /tmp/mask.png pets/fox/photo-splats.ply 200000
deskpet align-splats pets/fox --style photo                                  # fit scale, heading, offset
```

SHARP's model weights are licensed for research only (see apple/ml-sharp LICENSE_MODEL), so
`photo-splats.ply` is a research sample: don't ship it in a product.
