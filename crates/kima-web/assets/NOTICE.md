# Third-party assets in kima-web

| Asset | Origin | Licence |
|---|---|---|
| `fonts/montserrat-latin.woff2` | Byte-identical copy of upstream kima-hub `frontend/public/fonts/montserrat-latin.woff2` (verified unchanged at 9450c94b, 2026-11-18). Montserrat by Julieta Ulanovsky et al. | SIL Open Font License 1.1 (upstream licence of Montserrat). The OFL allows redistribution and embedding; the font must not be sold on its own. The file itself carries no licence text in this repository, so the OFL attribution lives here. |
| `kima.webp` | Byte-identical copy of upstream kima-hub `frontend/public/assets/images/kima.webp` (the Kima logo mark; verified unchanged at 9450c94b, 2026-11-18) | Part of this project: GPL-3.0 |
| `src/icons_data.rs` | Path data of 52 icons generated from `lucide-react` 0.552 by `tools/gen-kima-web-icons.mjs` | ISC (portions MIT, Cole Bemis 2013-2023, Feather); copyright Lucide Contributors 2025 |
| `kima-base.css` / `kima.css` | Tailwind CSS v4 output compiled from styles ported from upstream kima-hub's frontend by `tools/gen-kima-web-css.mjs` | Tailwind CSS is MIT; the project styles are GPL-3.0 |

ISC licence text of Lucide: <https://github.com/lucide-icons/lucide/blob/main/LICENSE>.
Montserrat OFL 1.1: <https://openfontlicense.org/> and <https://github.com/JulietaUla/Montserrat>.
