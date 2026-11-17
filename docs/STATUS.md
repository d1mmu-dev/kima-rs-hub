# kima-rs-hub — stan prac

_Ostatnia aktualizacja: 2026-11-17 · `main` = `30d8d16`_

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
| **M-PROF** | **Profile w koncie (Netflix)** | ✅ na `main`; ręczne logowanie właściciela odłożone do wdrożenia |
| M5 | Vibe (podobne utwory, nastrój) | 🟡 Task 0–18 zaimplementowane lokalnie; Task 17–18 czekają na niezależny przegląd, odbiór real-model otwarty |
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

## M-PROF — na main

Backend i kima-web (Task 0–16) scalone do `main` w `30d8d16`; CI `44028641367` zakończone sukcesem. Profile, izolacja danych, prywatność nazw, przełączanie i zarządzanie tokenami są wdrożone. Szczegółowe bramki i zaakceptowane odroczenia: plan M-PROF.

Decyzje właściciela (2026-11-14): awatary z 12 presetów; kilka profili dziecięcych; profilami zarządza tylko profil główny; usunięcie profilu kasuje jego dane; Subsonic hasłem → profil główny, inne profile przez swoje tokeny; publiczne playlisty innych profili konta widoczne; **nazwa profilu widoczna tylko w obrębie konta** (inne konta widzą sam login).

Ręczne sprawdzenie istniejącego konta właściciela odłożone do wdrożenia (nie PASS); ścisły smoke referencyjnego frontendu odłożony do M10. Testy wymagające prywatnych modeli pozostają poza zweryfikowanym zakresem.

## M5 — punkt integracyjny

**Najnowszy etap — Task 18 (2026-11-17):** predykat `vibeEmbeddings` (spec §5.8): CLAP nie wyłączony, zweryfikowana sonda feel oraz natywne audio o dokładnej tożsamości `MATCHING_SOUND` albo istniejący kwalifikujący się wektor; obecny hf_audio nigdy nie włącza vibe. Macierz degradacji §4.4 przez prawdziwy router: brak audio/wyłączony CLAP — istniejące wektory działają; brak modelu tekstowego — tylko search 503; brak sondy — similar 404, alchemy/path 400, mapa bez nieaktualnego układu, search działa; brak głów nastroju — `unanalyzed`, podobieństwo bez zmian; biblioteka hf_audio — search 503 „Vibe search needs the music CLAP model” (D-182). Serwer startuje bez zasobów vibe, pozostałe możliwości bez zmian. Harness real-model E2E (30 generowanych WAV, `docs/testing/vibe-e2e.md`): **ZAIMPLEMENTOWANY, NIEURUCHOMIONY**, odmawia bez środowiska i dla hf_audio przed połączeniem z bazą. Minor Task 17: zewnętrzny zaufany manifest tokenizera (rewizja + SHA-256 plików, zgodny z przypiętym Dockerfile upstreamu), weryfikacja przed użyciem, zapis tożsamości w referencji i porównanie w `readings_real`. **296 PASS / 0 FAIL / 8 ignored** w wybranych testach API/ident; workspace check, stable Clippy plain+pg-publish, fmt, SQLx prepare, pin/profile/diff: PASS. Czeka na niezależny przegląd; bez pobierania modeli, włączania flag w produkcji, push/merge/wdrożenia i Task 19–20. To nie pełny runtime workspace ani hosted CI.

**Etap — Task 17 (2026-11-17):** odczyty CLAP zero-shot z bieżących wektorów muzycznych, bez ponownego dekodowania audio, przed statystykami nastroju. Dokładne 24 prompty z przypiętego analyzera, niezależny syntetyczny referencyjny Python, softmax T=15, stałe Platta i zgodne z Pythonem zaokrąglenie do 3 miejsc. Cache single-flight związany z tożsamością modeli/tokenizera; batch do 500, kontrola embedded_at i kolejność blokad track→vector. RED deadlock 40P01 i anulowanie na blokadzie fazy nastroju odtworzone, poprawione; brak późnych zapisów w testach anulowania. Brak modelu tekstowego pomija wyłącznie odczyty, nie tworzy błędów etapów; brak odczytów pozostawia statystyki NULL. **482 PASS / 0 FAIL / 8 ignored** w wybranych niezdublowanych testach ident/DB/API/DTO/vibe; workspace all-target check, stable Clippy plain+pg-publish, fmt, SQLx prepare, pin/profile/diff: PASS. D-187 i `docs/testing/vibe-zero-shot.md` opisują osobny harness 20 prawdziwych utworów: **ZAIMPLEMENTOWANY, NIEURUCHOMIONY**, tolerancja 1e-3. Obecny hf_audio/text_model nie jest checkpointem muzycznym i jest jawnie odrzucany; wymagane są zgodne natywne modele, legalne prywatne zasoby i osobna zgoda. Real recall Task 14 nadal OPEN/NIEURUCHOMIONY. Task 17 czeka na niezależny przegląd; bez zmian migracji, flag, danych właściciela, live aplikacji, pobierania modeli, push/merge/wdrożenia i Task 18. To nie pełny runtime workspace ani hosted CI. Task 16 i poprawka coercion przeszły wcześniejszy niezależny przegląd bez Critical/Important/Minor.

Poniżej historia poprzednich etapów.

**Najnowszy etap — Task 16 + Minor Task 15 (2026-11-17):** reranking podobnych artystów po scaleniu źródeł używa istniejącego matching state, profili artystów i artistMatch: delta ±0,1, clamp 0–1, stabilne remisy, brak korekty bez danych; błąd bazy zachowuje oryginalną listę z inLibrary=false. Seed: MBID albo dokładna nazwa (bez fallbacku po nieznanym MBID); wyniki: MBID, potem nazwa. D-189 opisuje różnice natywnego storage/capability i niezmieniony resolver odczytów. Poprawka Minor Task 15 w osobnym lokalnym `43fdaa3`: numeryczne stringi jak JavaScript (hex/binary/octal, znaki, FEFF, Infinity, NaN, -0), truthiness przed konwersją; celowe ignorowanie nienumerycznej długości pozostaje. RED routera: 20 zamiast 16 dla `0x10`; RED wywołania enrichment oraz mutacja delta-zero wykryte i zachowane w dowodach. **441 PASS / 0 FAIL / 7 ignored** w wybranych niezdublowanych testach; workspace all-target check, stable Clippy plain+pg-publish, fmt, SQLx prepare check, profile/pin/diff: PASS. Task 15 oceniony 0 Critical/Important + 1 Minor; poprawka i Task 16 czekają na niezależny przegląd. Real-model recall **NIEURUCHOMIONY**, zgodny licencjonowany checkpoint muzyczny nadal niedostępny. Bez zmian danych osobistych, flag, live aplikacji, modeli, push/merge/wdrożenia; Task 17–20 nierozpoczęte. To nie pełny runtime workspace ani hosted CI.

**Poprzedni etap — Task 15 (2026-11-17):** zaimplementowane i lokalnie zweryfikowane similar/alchemy/path: dokładne błędy i kolejność walidacji, limity, caps i skala wyników, edytowane tytuły, płaskie DTO ścieżki. Globalne odczyty identyczne dla main/kids/standard dwóch kont, auth obowiązkowy, flagi nadal wyłączone. DTO porównane z interfejsami TypeScript referencji bez zmian frontendu. **247 PASS / 0 FAIL / 4 ignored** w wybranych niezdublowanych testach API/DTO/DB/ident/vibe-core (nie pełny runtime workspace). Workspace all-target check, stable Clippy plain+pg-publish, fmt, SQLx prepare check, pin/profile keys i diff check: PASS. D-193 opisuje UUID/nienumeryczne długości/native covers. Przegląd implementacji Task 14: 0 Critical/Important; osobny Minor normalizacji recall `0732404` i Task 15 wymagają niezależnego przeglądu. Real-model recall pozostaje **NIEURUCHOMIONY**, zgodny music checkpoint nadal niedostępny. Brak push/merge/wdrożenia; Task 16–20 nierozpoczęte. Poniższe akapity to historia wcześniejszych etapów.

Lokalna gałąź zachowuje Task 0–12 oraz profile i podcasty z `main`. Dwie kolejności aktualizacji migracji 0040/0060, świeża baza i rollback profili: zielone; oryginalne sumy wszystkich 40 migracji bez zmian. Wybrane 17 zestawów DB/ident/API: **247 PASS, 0 FAIL, 2 ignored**. Workspace check i stable Clippy (także pg-publish), SQLx prepare check, fmt, pin i klucze profili: PASS. To nie pełny odbiór M5: pełny runtime workspace, CI i prawdziwy model pozostają otwarte. Integracja zaakceptowana przez niezależny przegląd bez Critical/Important.

Task 13: zaimplementowane `/api/vibe/map`, `/api/vibe/mood-summary`, `/api/vibe/mood/{id}` (auth, DTO, snapshot nastrojów, gzip z hashem treści zamiast niepełnych timestampów). Test globalnego rerun chroni pełne dane profili kids i standard, nie tylko głównego; mutacja kasująca niegłówne stars została wykryta. **314 PASS / 0 FAIL / 3 ignored** w 24 wybranych zestawach; benchmark 100 tys. uruchomiony osobno: warm gzip 1,477 s, identity 1,427 s, cold gzip 2,540 s. Pomiar natywnego routera z wybranymi pakietami opt-level 3, nie przeglądarki/release; cold gzip przekracza cel warm-cache. Workspace check/Clippy default+pg-publish, SQLx prepare, fmt/pin/profile keys: PASS. Flagi funkcji pozostają wyłączone; bez push i zmian uruchomionej aplikacji. Poprawka Minor negocjacji kodowania została osobno zapisana w `207ed5d`.

Task 14: tekstowe `/api/vibe/search` z rzeczywistym adapterem CLAP, limitem 15 s, DTO i błędami 503/504/500; testy używają osobnego adaptera syntetycznego. Właściciel zatwierdził konstrukcję HNSW200 na podstawie diagnostyki syntetycznej i upstreamu, niezależnie od niezmierzonego recall real-model. **Nowa migracja 0041**: m16/construction200 + transakcyjny REINDEX; wszystkie 40 historycznych migracji bez zmian. Świeża baza, aktualizacja 0040+0060→0041 (pełne wiersze wszystkich tabel, bajty wektora, exact distance i rzeczywista przebudowa/użycie indeksu) oraz obie kolejności M5/profili: PASS. Oryginalny ścisły test 5 tys. wektorów/20 zapytań bez zmian przeszedł w trzech niezależnych budowach: **60/60 identycznych list**, po 21 skanów HNSW; to obserwacja, nie gwarancja recall. **304 PASS / 0 FAIL / 7 ignored** w wybranych, niezdublowanych testach DB/API/DTO/ident/vibe-core; powtórzenia ANN raportowane osobno. Workspace check i stable Clippy default+pg-publish, fmt, SQLx prepare check, pin/profile keys: PASS. Task 14 gotowy do niezależnego przeglądu; pełny runtime workspace i hosted CI nieuruchomione w tym etapie. **Otwarty odbiór:** recall@20 ≥0,95 dla 50 prawdziwych zapytań tekstowych na zgodnym modelu i legalnie dostępnych zasobach. Około **15 tys. utworów** to informacja właściciela do planowania, nie zmierzona liczba zindeksowanych utworów ani jakość. Bez pobierania modeli, wdrożenia, push/merge i bez rozpoczęcia Task 15.

Poprawka po przeglądzie Task 14: **harness real-recall zaimplementowany, NIEURUCHOMIONY** (`docs/testing/vibe-recall.md`). Wspólny SQL exact/raw HNSW bez fallbacku, weryfikowany natywny adapter, odmowa przy rozbieżności checkpointu/tożsamości bazy, snapshot read-only, hashe rzeczywistego korpusu i 50 zapytań, EXPLAIN ANALYZE na każde zapytanie oraz prywatny raport wektorów/recall/latencji. Obecny `hf_audio` nadal blokuje uruchomienie przed połączeniem; brak modelu, licencji i zgody na kopię rzeczywistych embeddings nie jest PASS. Minor trim: pełny zestaw ECMAScript (FEFF tak, 0085 nie), zachowane UTF-16/400 i prefiksowe limity. **82 PASS / 0 FAIL / 2 ignored** w 14 sekcjach testów (API 12, DB 7, DTO 57, harness syntetyczny 6); to węższy ponowny gate, nie nowy pełny runtime workspace. Workspace check/Clippy stable plain+pg-publish, fmt, SQLx prepare, pin/profile keys: PASS. Niezależny ponowny przegląd i rzeczywisty recall pozostają otwarte; Task 15 nierozpoczęty.

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

- 2026-11-18: M5 Task 18 po przeglądzie (0 Critical/Important): 8 uwag Minor poprawionych lokalnie (test tożsamości audio dla 202, utwardzony harness E2E: piny modeli przed bazą, KIMA_NET_DENY_REAL, pusta baza, sprzątanie po panice; pliki tokenizera przypięte w readings_real, skrypt Pythona bez assert, spec §5.8 i D-182 uzgodnione); bez wdrożenia, Task 19 nierozpoczęty.
- 2026-11-17: M5 Task 18 lokalnie gotowy do przeglądu: predykat vibeEmbeddings, macierz degradacji §4.4, harness real-model E2E (nieuruchomiony), proweniencja tokenizera; 296 PASS / 0 FAIL / 8 ignored, bramki statyczne zielone; bez wdrożenia.
- 2026-11-17: M5 Task 15 lokalnie gotowy do przeglądu: similar/alchemy/path, 247 PASS / 0 FAIL / 4 ignored, bramki kompilacji i statyczne zielone; real-model i hosted CI nadal otwarte, bez wdrożenia.

- 2026-11-15: M-PROF backend gotowy na gałęzi (Task 1–9); decyzja o prywatności nazw profili.
- 2026-11-14: **M6a na `main`** (CI zielone): fixtures, flaga `podcasts`, cache odcinków z limitami, naprawa cache okładek.
- 2026-11-14: M6a gotowe na `feature/m6-fixtures` (`4ef8f97`): fixtures, flaga, dokumentacja, J3/J6, limity pobierania; przegląd czysty, 2884 testów zielonych.
- 2026-11-14: `feature/m6-fixtures`: `cf90efe` fixtures, `81ccae0` poprawki po przeglądzie, `02247f6` domknięcie dokumentacji + smoke; naprawa J3/J6 w toku.

- 2026-11-13: M-PROF dopisane do roadmapy; decyzje profili.
- 2026-11-13: `5f4c4ff` D-205 404; `4c04c3b` filtr AI (2856 testów zielonych, CI zielone).
- 2026-11-13: `d1666b1` M6 odświeżanie + strony podcastów w kima-web (inna sesja).
- 2026-11-12: `c01c8a7` M-STATS S1.
- 2026-11-11: `17b6915` M-SYNC.
