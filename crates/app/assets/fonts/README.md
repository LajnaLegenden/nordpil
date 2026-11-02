# Fonts

- `Geist.ttf` and `GeistMono.ttf`: [Geist](https://github.com/vercel/geist-font)
  variable fonts from [Google Fonts](https://github.com/google/fonts/tree/main/ofl/geist),
  under the SIL Open Font License (`OFL-Geist.txt`).
- `lucide.ttf`: the icons nordpil uses from [Lucide](https://lucide.dev) 1.65.0
  (`lucide-static` on npm), under the ISC license (`LICENSE-lucide.txt`).

`lucide.ttf` is a subset of `lucide-static/font/lucide.ttf`, made with
[fonttools](https://github.com/fonttools/fonttools). To add an icon, look up its
code point in `lucide-static/font/codepoints.json`, add it to the list below and
to `src/icons.rs`, then run:

```sh
pyftsubset lucide-static/font/lucide.ttf --no-hinting --output-file=lucide.ttf \
  --unicodes="U+E124,U+E13D,U+E1B2,U+E06F,U+E06D,U+E06E,U+E070,U+E0AD,U+E529,U+E0BA,U+E0BB,U+E087,U+E10B,U+E0CC,U+E0FD,U+E0DC,U+E245,U+E1F5,U+E151,U+E106,U+E36A,U+E17D,U+E19E,U+E0B2,U+E1B4,U+E145,U+E1F9,U+E09E,U+E18E,U+E37F,U+E45D,U+E154,U+E06C,U+E0A1,U+E176,U+E077,U+E193,U+E0B6,U+E0F9,U+E0D9,U+E0C9,U+E148,U+E153,U+E084,U+E04A,U+E042,U+E13C,U+E172,U+E20A,U+E167,U+E14D,U+E247"
```
