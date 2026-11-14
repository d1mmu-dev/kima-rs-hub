# kima-rs-hub — stan prac

_Ostatnia aktualizacja: 2026-11-14 · M6a na `main`_

<!-- private -->
> Podsumowanie stanu projektu (aktualizowane po każdym etapie, publikowane razem z kolejnym merge'em do `main`). Źródłem prawdy jest historia gita; szczegóły różnic od upstreamu są w `docs/DIVERGENCES.md`, plan w `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md`.
<!-- /private -->

## Cel

Wierny port upstreamu **kima-hub v1.12.3** (pin `c7527577`) do Rusta; jedyny frontend produkcyjny to **kima-web** (Rust/WASM, vertigo, SSR). Wygląd docelowo 1:1 z upstreamem. Każda różnica od upstreamu → `docs/DIVERGENCES.md`.

## Postęp milestone'ów

| Etap | Zakres | Stan |
|---|---|---|
| M0 | Szkielet: workspace, DB, kolejka zadań, auth, Docker | ✅ |
| M1 | Biblioteka, skaner, odtwarzanie, playlisty | ✅ |
| M2 | Subsonic | ✅ |
| M3 | Identyfikacja (MusicBrainz, AcoustID), autotagger | ✅ |
| M4 | ML: CLAP (MusiCNN później) | ✅ (tylko CLAP) |
| M-SYNC | Przejście na upstream v1.12.3 | ✅ |
| M-STATS S1 | Statystyki odsłuchów | ✅ (S2 po analizie audio) |
| M9 | kima-web część 1 (logowanie, biblioteka, player, playlisty, ustawienia, SSR) | ✅ (wygląd 1:1 otwarty) |
| M6a | Podcasty + filtr AI + strony kima-web | ✅ (porównanie wizualne stron → M10) |
| **M-PROF** | **Profile w koncie (Netflix)** | 🟡 **następny, ~2–3 tyg.** |
| M5 | Vibe (podobne utwory, nastrój) | ⏳ gałąź istnieje, ~1 tydz. |
| M5b | Mixy, radio, „co nowego” | ⏳ ~1 tydz. |
| M7 | Integracje (Lidarr, Soulseek, discover…) | ⏳ 2–3 tyg. |
| M6b | Audiobooki (Audiobookshelf) | ⏳ ~1 tydz., po M7 |
| M8 | Utwardzenie: 2FA, klucze API, share, limity | ⏳ 1–2 tyg. |
| M10 | kima-web część 2: pozostałe strony + wygląd 1:1 | ⏳ 3–4 tyg. |
| M11 / M12 | Chromecast/AirPlay; zdalne ML na GPU | ⏳ po ~1 tydz. |

**Szacunek do pełnej aplikacji wg planu: 2,5–3,5 miesiąca** (orientacyjnie, zależy od limitów modeli i decyzji).

Kolejność: **M-PROF** → M5 → M5b → M7 → M6b → M8 → M10 → M11/M12.

## M6a — podsumowanie

Na `main`: wszystkie 24 trasy upstreamu `/podcasts` + trasy etykiet AI, wyszukiwanie i discover, proxy okładek, ostatnio słuchane i statystyki, Subsonic, odświeżanie kanałów (worker + sweep co 15 min), cache odcinków (najpierw z cache, pobieranie w tle z limitami rozmiaru i czasu, sprzątanie), uzupełnianie kategorii przy starcie, strony kima-web, filtr AI, fixtures API przechwycone z prawdziwego serwera, `podcasts: true`.

Odłożone (nieblokujące): porównanie wizualne stron podcastów (M10); `/search` filtruje AI po `LIMIT`; przełącznik AI w Ustawieniach startuje od `false`; brak odznaki „E” (explicit); błędny Range na pliku z cache → 200 zamiast 416 (D-214).

## M-PROF — następny etap

Do napisania: plan (`docs/superpowers/plans/`), potem implementacja. Szczegóły i decyzje w sekcji niżej.

## Decyzje właściciela (wiążące, nie pytać ponownie)

- Filtr AI: tag Podcasting 2.0 + nadpisanie admina; odcinek ma pierwszeństwo; brak danych = „nieznane” i widoczne; AI domyślnie ukryte z przełącznikiem (D-200).
- Audiobooki = M6b po M7. Strony podcastów w kima-web w M6.
- D-205: odświeżanie przy SSRF/braku odpowiedzi → 404 jak upstream.
- **M-PROF:** do 5 profili (główny + dzieci + 3); profil dziecięcy na razie **bez filtrów** (dobór filtrów później, po uruchomieniu oznaczeń AI); **bez PIN-u teraz** (przełączanie gotowe na opcjonalny PIN później, ~0,5–1 dnia); tokeny Subsonic per profil; biblioteka wspólna; admin = profil główny. D-240..D-249, migracje 0060–0069.
- Porównanie wizualne stron podcastów z oryginałem odłożone do M10 (2026-11-14).
- Next.js tylko jako referencja porównawcza, nigdy w produkcji/dev/CI.
- Push do `main` tylko po pytaniu (prośba właściciela o merge = zgoda); gałęzie feature można wypychać.
- Commity: `dimmu <3417783+d1mmu-dev@users.noreply.github.com>`, bez `Co-authored-by` i trailerów AI.

## Otwarte pytania

- M5: odbiór wymaga prawdziwego modelu CLAP `laion-clap-music-onnx-v1` (licencje/rozmiary do pokazania przed pobraniem).

<!-- private -->
## Zasady pracy

- Jeden pisarz na worktree.
- Testy DB tylko na izolowanej bazie; sprawdzaj tożsamość bazy przed każdym `cargo test`.
- Każdy etap: TDD, niezależny przegląd (0 Critical/Important), pełne bramki, zielone CI przed merge.

## Historia etapów (najnowsze na górze)

- 2026-11-14: **M6a na `main`** (CI zielone): fixtures, flaga `podcasts`, cache odcinków z limitami, naprawa cache okładek.
- 2026-11-14: M6a gotowe na `feature/m6-fixtures` (`4ef8f97`): fixtures, flaga, dokumentacja, J3/J6, limity pobierania; przegląd czysty, 2884 testów zielonych.
- 2026-11-14: `feature/m6-fixtures`: `cf90efe` fixtures, `81ccae0` poprawki po przeglądzie, `02247f6` domknięcie dokumentacji + smoke; naprawa J3/J6 w toku.

- 2026-11-13: M-PROF dopisane do roadmapy; decyzje profili.
- 2026-11-13: `5f4c4ff` D-205 404; `4c04c3b` filtr AI (2856 testów zielonych, CI zielone).
- 2026-11-13: `d1666b1` M6 odświeżanie + strony podcastów w kima-web (inna sesja).
- 2026-11-12: `c01c8a7` M-STATS S1.
- 2026-11-11: `17b6915` M-SYNC.
