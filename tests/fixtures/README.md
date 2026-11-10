`adobe-classic-ease.fla` is a generated 96×96 geometric tile, saved in Adobe
Animate 24 after assigning `frame.tweenEasing = 50` through JSFL. Its timeline
moves from `(100, 100)` to `(500, 250)` over 40 frames. Animate stores
`acceleration="-50"`; its exported frame 20 corresponds to `(350, 193.75)`.
The importer tests read the editable XFL directly and never restore the embedded
original compiled metadata. Keeping the actual Adobe save also covers Animate's
54-byte ZIP central-directory size overcount.

The three small JPEG fixtures cover RGB, grayscale, and CMYK decoding.

`adobe-raw-bitmap.dat` is the unchanged 346-byte `Chomper_stem3.png` bitmap
member from an Adobe ZIP FLA. Its final header byte is zero: the 26-byte header
is followed directly by premultiplied ARGB rows, without compressed chunks.
The adjacent JSON records the source archive/member hashes and a straight RGBA
pixel hash computed independently in Python. This covers a format used by 21
of the original Chomper FLA's 23 bitmaps; the compressed form sets that byte to
one. Generated tests additionally cover padded rows and malformed raw/chunked
lengths. No original full FLA is included for this bitmap fixture.

`goty-anim-reference.json` compares two independent original PopCap sources.
Its XFL members are unchanged marker-layer subtrees from the stock GOTY FLA
files after Adobe Animate 24 saved them as ZIP FLA. Its expected track order and
active ranges were separately extracted from the stock `reanim/*.reanim` text
files shipped beside `FlashReanimExport.jsfl`. It covers Blover, Chomper, Zombie,
Zombie_boss, Zombie_polevaulter and SelectorScreen (61 image-free `anim_` tracks).
Both input hashes are recorded for each case. Only the marker layers and their
library dependencies are retained, without images or compiled recovery metadata.
The test calls the ordinary Rust forward importer directly. Other tests cover
hidden artwork, helper layers, root element counts and attachment commands.

`goty-blover-symbol-tween.json` retains the original Blover petals layer and its
symbol XML, plus the three petals tracks' independently expanded x/y/f values
from the stock `.reanim`. This covers offset children moving in arcs when their
outer symbol rotates; interpolating flattened bitmap endpoints produced errors
over 28 pixels. Numeric comparisons allow the smaller differences caused by
modern Animate interpolation and the original JSFL numeric formatting.
