# WarpForge — Firmware-Design, Stufe 1: BM1370

*Arbeitstitel. Stand 2026-09-22. Autor: Oliver + Claude. Status: Entwurf, nichts davon gebaut.*

---

## 0. Scope

**Stufe 1 = BM1370, ein Chip, Bitaxe Gamma 60x.** Ein Gerät, eine Topologie, ein
Referenzboard. Alles andere ist Ballast, solange der erste akzeptierte Share nicht
steht.

**Nicht-Ziele in Stufe 1:**

- BM1373 → wartet auf die Gaia-Hardware (NerdAxe Gaia, bitronics.store). Die
  Architektur hält den Platz frei (Chip-Profil), aber es wird kein Profil auf
  Verdacht geschrieben.
  **Am Gerät gemessen 2026-09-22** (NerdOS `v1.1.0-31-gae509770`, `/api/system/asic`
  + `/api/system/info`, 192.168.178.40):

  | | BM1373 (Gaia) | BM1370 (Gamma 602) |
  |---|---|---|
  | Chips | 1 | 1 |
  | `smallCoreCount` | **6860** | 2040 |
  | Stock | **350 MHz @ 940 mV** | 525 MHz @ 1150 mV |
  | Frequenz-Optionen | 300,320,340,**350**,360,380,400,420,440 | … bis 575 |
  | Spannungs-Optionen | 900…1040 mV (20er) | … |
  | abs-Grenzen | V 800–1300 mV, f **kein Limit** (`absMaxFrequency: 0`) | |
  | Board | 12 V (10,8–13,2), max 6 A, `maxPower` 40 W | 5 V |

  `6860 × 350 MHz = 2401 GH/s` — die 2,4 TH/s der Herstellerangabe rechnen exakt auf.

  **Die eigentliche Erkenntnis: BM1373 ist breit und langsam, BM1370 schmal und
  schnell.** 3,4× so viele Cores bei 2/3 der Taktrate. Für das Chip-Profil heißt
  das ein völlig anderer PLL-Arbeitsbereich — nichts davon lässt sich vom BM1370
  ableiten, und eine gemeinsame „BM137x"-Leiter wäre falsch.

  **Beleg für §3.3 (PLL lösen statt Tabelle):** Die Frequenzliste ist *nicht*
  gleichmäßig — 20er-Schritte, aber um den Stock-Punkt herum 340/350/360 in
  10er-Schritten. Das ist genau das Muster einer Liste, die aus dem gebaut wurde,
  was die PLL tatsächlich trifft. Mit einem Löser hätte man das Raster, statt es
  zu raten.

  Weitere Folgerungen: (a) **Single-Chip wie der Gamma** — n=1 bleibt der
  Normalfall; (b) das 1,9"-IPS-Display braucht einen eigenen HAL-Trait
  (`Display`, SPI) plus UI-Task, der **nie** im Mining-Pfad blockiert;
  (c) ⚠️ **`asicTemps: [0]` — es gibt keine ASIC-Junction-Temperatur.** Die
  einzigen echten Sensoren sind `temp` (Board) und `vrTemp`. Das ist exakt der
  Fall aus §7.4: eine Firmware, die hier auf einen Sensor regelt, der nichts
  misst, hat keinen thermischen Schutz. Die Plausibilitätsprüfung ist kein
  Luxus, sondern Pflicht.

  Noch ungeprüft: MCU-Variante, VR-Typ, tatsächliche PLL-Divider-Grenzen.
- BM1366 / BM1368 → **endgültig raus** (2026-09-22): nicht im Portfolio, also
  nie gegen Hardware prüfbar. Siehe §12.1.
- Mehrchip-Ketten (NerdQAxe++, Octaxe) → die Adressierung wird von Anfang an
  n-chip-fähig entworfen, aber nur mit n=1 getestet und ausgeliefert.
- Nichts Kommerzielles. Das hier ist Lernprojekt + Werkzeug, keine Produktlinie.

**Das eine Ziel, an dem sich alles misst:** Eine Firmware, deren Verhalten man
*ohne Hardware* in CI reproduzieren kann. Alles andere in diesem Dokument folgt
daraus.

---

## 1. Plattform-Entscheidungen

### 1.1 MCU: ESP32-S3 bleibt

Nicht verhandelbar, weil die Boards existieren. Ein besserer MCU nützt nichts,
wenn keiner die Firmware flashen kann.

### 1.2 Sprache: Rust auf ESP-IDF (`std`), nicht `no_std`

| Option | Urteil |
|---|---|
| C auf ESP-IDF (wie ESP-Miner) | Verworfen. Kein Gewinn gegenüber dem Bestand, und die Bugklassen, die dort weh tun (Buffer, Lifetimes, ignorierte Fehlerrückgaben), bleiben. |
| Rust `no_std` + `esp-hal` + `embassy` | Verworfen für Stufe 1. Schön, aber WiFi/TLS/OTA müsste man neu bauen oder `esp-wifi` in Kauf nehmen. Das ist Risiko an der falschen Stelle. |
| **Rust `std` auf `esp-idf-svc`/`esp-idf-hal`** | **Gewählt.** WiFi, mbedTLS, NVS, OTA, HTTP-Server kommen vom IDF. Der Mining-Kern ist trotzdem `no_std`-sauber und damit host-testbar. |

Der Punkt ist nicht „Rust ist besser". Der Punkt ist: Der Teil, der zählt
(`forge-core`), ist dann eine reine Bibliothek ohne Hardware-Abhängigkeit, und
den kann man auf dem Mac in Millisekunden durchtesten statt auf dem Board in
Minuten.

### 1.3 Toolchain

- `espup` + `esp` Rust-Toolchain (Xtensa), `ldproxy`
- Build-Matrix: `forge-core` auf **stable host** (x86_64/aarch64) für Tests,
  Gesamtfirmware auf Xtensa
- Kein `cargo-espflash`-Zwang im CI — das Firmware-Image wird gebaut und
  artefaktiert, geflasht wird vom HIL-Runner (§10.3)

---

## 2. Modulschnitt

```
forge-core/          no_std, no alloc, keine HW-Traits
  ├ frame/           0x55AA-Rahmen, CRC5, CRC16, Encode/Decode
  ├ chip/            ChipProfile-Typen, PLL-Löser, Registerkonstanten
  ├ job/             Stratum-Job → ASIC-Work, extranonce, version rolling
  ├ nonce/           Nonce-Response → (chip, core, share), Difficulty-Prüfung
  └ sha/             double-SHA256 zur lokalen Share-Verifikation

forge-proto/         no_std + alloc, keine HW
  ├ v1/              Stratum V1: inkrementeller Parser + Encoder
  └ v2/              Stratum V2: Noise NX + Mining-Protokoll  (Stufe 3)

forge-control/       no_std + alloc, keine HW, keine Uhr (Zeit wird injiziert)
  ├ tune/            Betriebspunkt-Automat (Settle → Judge → Commit)
  ├ thermal/         Thermikmodell + Lüfterregelung
  └ health/          Fehlerbudget, Degradationsstufen

forge-hal/           Traits: Uart, Gpio, I2c, Clock, Rng, Storage
forge-esp32s3/       Implementierung der Traits auf esp-idf-hal
forge-app/           Tasks, Verdrahtung, HTTP/WS-API, Web-Assets
forge-sim/           Host-Binary: BM1370-Simulator (§6)
forge-tests/         Integrationstests: forge-app-Logik gegen forge-sim
```

**Die Regel, die das Ganze zusammenhält:**
`forge-core`, `forge-proto` und `forge-control` dürfen **keine** HW-Crate, keine
`std::time`, kein `log!` mit Seiteneffekt kennen. Zeit kommt als `now_ms: u64`
rein, Zufall als Seed, I/O als Rückgabewert („ich will diese Bytes senden"),
nie als Aufruf. Das ist exakt das Muster aus `warppool_tune::regulator` —
pure decision logic, no I/O — und es funktioniert dort.

**Konsequenz:** ~80 % der Firmware-Logik läuft in `cargo test` auf dem Mac.

---

## 3. Chip-Profil: Registermap als Daten

### 3.1 Das Problem, das wir vermeiden

ESP-Miner hat `bm1366.c`, `bm1368.c`, `bm1370.c` — dreimal fast derselbe Code.
Ein Fix in einer Datei landet nicht automatisch in den anderen. (Dieselbe
Bugklasse wie V1/V2 in WarpPool, nur ohne die Disziplin, beide zu prüfen.)

### 3.2 Die Form

```rust
// forge-core/src/chip/profile.rs
pub struct ChipProfile {
    pub name: &'static str,

    // Identität
    pub chip_id: u16,              // Antwort auf GET_STATUS 0x00
    pub id_mask: u16,

    // Topologie
    pub small_cores: u16,          // BM1370: 2040 (aus esp-miner; zu verifizieren)
    pub big_cores: u8,
    pub domains: u8,

    // Takt
    pub xtal_hz: u32,              // 25_000_000
    pub pll: PllSpec,              // Divider-Grenzen, siehe 3.3
    pub freq_min_mhz: u16,
    pub freq_max_mhz: u16,         // harte Sperre, nicht "empfohlen"

    // Initialisierung
    pub init: &'static [InitStep],

    // Protokoll
    pub nonce: NonceLayout,
    pub version_rolling: VersionRollingCaps,
    pub uart: UartSpec,

    // Registeradressen (Familienunterschiede!)
    pub reg: RegMap,

    // Physik
    pub thermal: ThermalSpec,
    pub electrical: ElectricalSpec,
}
```

`InitStep` ist bewusst eine **Datenliste mit Semantik**, keine Bytefolge:

```rust
pub enum InitStep {
    Write   { reg: u8, val: u32 },
    WriteAll{ reg: u8, val: u32 },
    Read    { reg: u8, expect: Option<u32> },  // expect=Some → Abbruch bei Abweichung
    CoreReg { id: u8, val: u8 },
    ChainInactive,
    SetAddress { step: u8 },
    Delay   { ms: u16 },
    Baud    { rate: u32 },
}
```

**Warum mit Semantik statt roh:** Der Simulator kann eine Sequenz dann
*validieren* („du hast SET_ADDRESS geschickt, bevor CHAIN_INACTIVE kam — das
macht echte Hardware nicht mit") statt nur Bytes zu vergleichen. Und `Read` mit
`expect` macht die Init zur selbstprüfenden Sequenz: Wenn der Chip nach dem
PLL-Write nicht den erwarteten Wert zurückliest, brechen wir *dort* ab, statt
zehn Minuten später über Hardware-Errors zu rätseln. ESP-Miner schreibt
überwiegend blind.

### 3.3 PLL: lösen statt Tabelle nachschlagen

```
f_out = f_xtal × fbdiv / (refdiv × postdiv1 × postdiv2)
```

```rust
pub struct PllSpec {
    pub fbdiv:    RangeInclusive<u16>,
    pub refdiv:   RangeInclusive<u8>,
    pub postdiv1: RangeInclusive<u8>,
    pub postdiv2: RangeInclusive<u8>,
    pub vco_hz:   RangeInclusive<u64>,   // f_xtal × fbdiv / refdiv muss hier rein
    pub constraints: &'static [PllConstraint],  // z.B. postdiv1 >= postdiv2
}
```

`solve(profile, target_mhz) -> Option<PllWord>` sucht die Kombination mit dem
kleinsten Fehler, die *alle* Constraints erfüllt, und gibt die tatsächlich
erreichte Frequenz mit zurück.

Zwei Dinge, die das besser macht als heute:

1. **Die UI zeigt die echte Frequenz, nicht die gewünschte.** Wenn du 545 MHz
   einstellst und die PLL nur 543,75 MHz kann, dann steht 543,75 da. Heute lügt
   die Anzeige, und jede Effizienzrechnung (J/TH) lügt mit.
2. **Die Frequenzleiter wird generiert, nicht hartcodiert.** Das
   `STEP_MHZ = 25`-Raster aus WarpTune ist eine Näherung an das, was die PLL
   tatsächlich treffen kann. Mit dem Löser bekommst du die *erreichbaren* Punkte —
   und zwischen 525 und 550 liegen mehr davon als eine 25er-Leiter zeigt.

Ein `const`-Test prüft in CI, dass jeder Punkt der Leiter lösbar ist und dass die
gelöste Frequenz innerhalb ±0,5 % liegt.

### 3.4 Quellenlage BM1370 — ehrlich

Alles, was wir über die Registermap wissen, stammt aus Reverse Engineering
(ESP-Miner `main/asic/bm1370.c`, NerdQAxe-Fork, cgminer-Patches). Es gibt **kein**
Datenblatt.

Daraus folgt der Umgang damit:

- Jeder Registerwert im Profil bekommt ein Herkunfts-Attribut:
  `#[src("esp-miner v2.15.1 bm1370.c:142")]` oder `#[src("measured 2026-xx-xx")]`
- Werte ohne Messung gelten als **unbestätigt** und werden im Boot-Log als solche
  markiert
- Ein `verify`-Modus fährt beim ersten Boot einer neuen Firmware die
  Read-Back-Prüfung über alle beschreibbaren Register und protokolliert
  Abweichungen — das ist die Datenquelle, aus der die Map über die Zeit *belegt*
  statt geerbt wird

Sicher genug für den Start (aus dem Bestand, in CI gegen Fixtures gepinnt):

| Sache | Wert |
|---|---|
| Preamble | `0x55 0xAA` |
| Command-Prüfsumme | CRC5 (Bitmain-Variante, über Header+Payload) |
| Work-Prüfsumme | CRC16 |
| Init-Baudrate | 115200 |
| Nonce-Response | `0xAA 0x55` + Nonce + Job-ID + Version-Bits + CRC5 |
| Version-Rolling-Maske | `0x1fffe000` |
| Ticket-Mask | lokale Difficulty-Schwelle, Zweierpotenz |
| Stock-Punkt Gamma | 525 MHz @ 1150 mV |

Alles darüber hinaus: extrahieren, pinnen, messen.

---

## 4. Wire-Layer

### 4.1 Frames

```rust
// Encode ist allokationsfrei in einen &mut [u8]
pub fn encode_cmd(buf: &mut [u8], cmd: Cmd) -> Result<usize, FrameErr>;

// Decode ist inkrementell: füttere Bytes, bekomme Events
pub struct RxDecoder { /* fester Ringpuffer, kein Heap */ }
impl RxDecoder {
    pub fn push(&mut self, byte: u8) -> Option<RxEvent>;
}
pub enum RxEvent {
    Nonce(NonceResponse),
    RegRead { chip: u8, reg: u8, val: u32 },
    Desync { dropped: u16 },     // ← existiert bei ESP-Miner nicht
}
```

`Desync` ist wichtig: Wenn der UART-Strom aus dem Tritt kommt (kommt vor, bei
hoher Baudrate und Störungen auf dem Kabel), soll die Firmware das **zählen und
melden**, nicht stillschweigend Bytes wegwerfen. Eine steigende Desync-Rate ist
ein Frühindikator für ein Signalintegritätsproblem, lange bevor die
Hashrate einbricht.

### 4.2 UART-Betrieb

- RX über DMA in einen Ringpuffer, ISR schiebt nur, Parsing im Task
- Baudrate-Wechsel als atomarer Schritt: Chip-Register schreiben → Drain →
  MCU-UART umstellen → Read-Back-Probe → bei Fehlschlag zurück auf 115200
  (ESP-Miner hat hier keinen Rückweg; ein fehlgeschlagener Baud-Switch heißt
  Reboot)
- TX-Budget: Kommandos an definierten Punkten, nie mitten in einem Work-Burst

---

## 5. Job-Pipeline und Speichermodell

### 5.1 Keine Allokation im Hot-Path

```
[WiFi/TCP] → [V1-Parser, inkrementell] → [Job-Arena, N=8 Slots, statisch]
                                              ↓ SPSC-Ring
                                        [Work-Builder]
                                              ↓
                                        [UART TX]

[UART RX] → [RxDecoder] → SPSC-Ring → [Share-Validator] → [Submit-Queue, persistiert]
```

Der Stratum-Parser ist ein **Zustandsautomat über den Bytestrom**, kein
`cJSON_Parse` über einen zusammengesetzten String. Er schreibt Felder direkt in
den Job-Slot und hat harte Obergrenzen für jedes Feld (merkle branches, coinb1/2,
extranonce). Ein Pool, der zu große Felder schickt, bekommt einen Fehler und
einen Disconnect — nicht einen Heap-Overflow.

Job-Arena: 8 Slots à fester Größe, Ringbelegung, Slot-Generation als Teil der
Job-ID. Ein Nonce, der zu einem überschriebenen Slot kommt, wird als
`stale_local` gezählt und **nicht** eingereicht. (Heute wird er teils eingereicht
und produziert Pool-Rejects, die dann fälschlich als Instabilität gewertet
werden — genau der Fall, den `is_benign_reject()` in WarpTune nachträglich wieder
ausfiltern muss.)

### 5.2 Mining überlebt Netzausfall

Bei Verbindungsverlust:

1. ASIC läuft auf dem letzten Job weiter, `ntime` wird im erlaubten Fenster
   gerollt
2. Gefundene Shares gehen in eine persistente Submit-Queue (NVS-Ring, ~64
   Einträge)
3. Nach Reconnect: Re-Subscribe, dann Nachreichen dessen, was noch im
   ntime/job-Fenster liegen kann

Nach 120 s ohne Verbindung wird der Job als tot betrachtet und der ASIC auf
einen niedrigen Punkt gefahren (Strom sparen statt sinnlos hashen).

### 5.3 Failover mit Hot-Standby

Zwei Pool-Slots, **beide** verbunden, subscribed, authorized. Der Standby
bekommt Jobs und verwirft sie. Umschalten = Pointer-Swap + ein Work-Push, kein
Handshake.

Kosten: ein zweiter TCP-Socket und ~4 KB. Gewinn: Failover ohne
Hashrate-Loch. Heute kostet ein Pool-Wechsel den vollen Reconnect-Zyklus.

### 5.4 Lokale Share-Verifikation

Jeder Nonce wird **vor** dem Submit lokal doppelt-SHA256-geprüft gegen die
Pool-Difficulty. Kostet auf dem S3 wenige µs pro Share (die Rate ist niedrig).
Gewinn: Ein Chip, der Müll liefert, produziert keine Pool-Rejects mehr, sondern
einen sauberen lokalen HW-Error-Zähler. Das trennt „Chip instabil" von
„Pool hat abgelehnt" an der Quelle — statt hinterher aus Reject-Strings raten
zu müssen.

---

## 6. Der Simulator — das eigentliche Kernstück

`forge-sim` ist ein Host-Binary, das sich wie ein BM1370 am UART verhält.

### 6.1 Transport

- **PTY-Modus:** legt ein `/dev/ttys00x` an, das die Firmware-unter-Test öffnet
- **In-Process-Modus:** `forge-sim` als Bibliothek, direkt gegen die
  `forge-hal::Uart`-Trait-Implementierung — das ist der schnelle Pfad für
  `cargo test`, kein OS im Spiel
- **TCP-Modus:** für manuelles Debuggen mit echtem Tooling

### 6.2 Was er modelliert

```rust
pub struct SimChip {
    profile: &'static ChipProfile,
    regs: [u32; 64],
    addr: Option<u8>,
    baud: u32,
    state: ChipState,      // Reset | Addressed | Configured | Hashing
    pll: PllWord,
    // Physik
    junction_c: f32,       // einfaches RC-Modell aus Leistung
    // Arbeit
    current_work: Option<Work>,
    rng: Pcg32,            // deterministisch! seed = Testparameter
}
```

Der Sim **hasht nicht wirklich**. Er zieht aus der Nonce-Rate, die sich aus
Frequenz × Cores × Ticket-Mask ergibt, deterministisch Zeitpunkte und liefert
Nonces, die zu dem Work passen — konstruiert, nicht gefunden. Für die
Firmware ist das ununterscheidbar, und der Test läuft in Millisekunden statt in
Stunden.

### 6.3 Fehlerinjektion — der Grund, warum sich das lohnt

Steuerkanal (separates Socket / API im In-Process-Modus):

```rust
pub enum Fault {
    NoResponse { for_ms: u32 },
    GarbageNonces { rate: f32 },
    BitFlipTx { probability: f32 },      // UART-Störung
    HangAfterFreqChange { above_mhz: u16 },
    BrownoutBelow { mv: u16 },           // liefert HW-Errors statt Nonces
    ThermalRunaway { rate_c_per_s: f32 },
    RegReadCorrupt { reg: u8 },
    BaudSwitchFails,
    ColdStartSlow { ms: u32 },
    DuplicateNonce { rate: f32 },

    /// ⚠️ NICHT ausgedacht — am Gaia eingetreten, 2026-09-22 11:44.
    /// VR-Ausgang auf 0, Leistung 36 W → 5 W, kein Hashing.
    /// KEIN Fault-Flag, KEIN Reboot, Registerzugriff bleibt beantwortbar,
    /// gemeldete Hashrate bleibt auf dem alten Wert stehen.
    /// Siehe 01-Nutzen-fuer-WarpTune.md, Abschnitt „Feldbeleg".
    SilentVrShutdown { above_mhz: u16 },
}
```

`SilentVrShutdown` ist der wichtigste Testfall der ganzen Liste, weil er die
bequemste Annahme widerlegt: dass ein totes Gerät sich als tot meldet. Der
einzige ehrliche Messwert war die Leistungsaufnahme. Erwartetes Verhalten:
Erkennung innerhalb eines Ticks, Punkt als schlecht markiert, Rückfall auf den
Anker, Eintrag im Post-Mortem-Log — ohne Handeingriff und ohne Netz.

Jeder davon ist ein Feldfehler, den heute niemand testet, weil man ihn auf echter
Hardware nicht auf Knopfdruck erzeugen kann. Mit dem Sim ist jeder ein
Regressionstest:

- „Chip antwortet 5 s nicht" → erwartet: Recovery-Sequenz, kein Reboot,
  Ereignis im Post-Mortem-Log
- „Hängt oberhalb 600 MHz" → erwartet: Tuner verankert 575 MHz, markiert 600 als
  schlecht, kommt eine Stunde nicht wieder (das Step-Memory-Verhalten aus
  WarpTune, nur eine Ebene tiefer)
- „Brownout unter 1050 mV" → erwartet: Spannung wird *vor* der Frequenz
  angehoben, nicht danach

### 6.4 Aufzeichnen und Abspielen

`forge-sim record` hängt sich (über ein Y-Kabel oder den Board-Debug-UART) an
echte Hardware und schreibt den Dialog mit. `forge-sim replay <trace>` spielt ihn
zurück. Damit wird jeder echte Feldfehler, den du einmal einfängst, zu einem
dauerhaften CI-Test.

**Das ist der Teil, der unabhängig vom Rest Wert hat** — siehe Teil 2 der
Durchrechnung.

---

## 7. Regelung

### 7.1 Aufgabenteilung Firmware ↔ Sidecar

Das ist die zentrale Designfrage, weil WarpTune die Leiter-Regelung schon hat.

**In der Firmware, weil nur dort möglich:**

| Funktion | Warum nicht im Sidecar |
|---|---|
| Rampen-Ordnung (V vor f hoch, f vor V runter) | Millisekunden, am Bus |
| PLL-Wechsel an Job-Grenzen | Millisekunden |
| Not-Aus bei Übertemperatur | darf nicht von WLAN abhängen |
| Rückfall auf letzten verankerten Punkt | muss ohne Netz funktionieren |
| Lüfterregelung | Regelkreis, 1 Hz, lokal |
| Per-Chip-Fehlerattribution | braucht Nonce-Rohdaten |

**Im Sidecar (WarpTune), weil dort besser:**

| Funktion | Warum |
|---|---|
| Suchstrategie über Betriebspunkte | braucht Historie, Vergleich über Geräte, Pool-Sicht |
| Energiemodus / Solar-Attribution | braucht Hauskontext |
| Langzeit-Journal | braucht Platz |
| Abstimmung über mehrere Geräte | braucht Gesamtsicht |

**Die Schnittstelle dazwischen ist ein Vertrag, kein Befehl:**

```
POST /api/v1/operating-point
{ "frequency_mhz": 575, "core_voltage_mv": 1190, "anchor": true, "ttl_s": 3600 }
```

- `anchor: true` → dieser Punkt wird nach erfolgreicher Settle-Phase in NVS
  verankert und ist von da an das Rückfallziel
- `ttl_s` → wenn das Sidecar stumm wird, fällt das Gerät nach Ablauf auf den
  letzten Anker zurück, **nicht** auf Werksdefaults

Das ist deine Philosophie direkt in der Firmware: Der Gerätepunkt ist die
validierte Obergrenze, und die Firmware hält ihn. Kein Sidecar, kein Netz, kein
Reboot darf ihn wegwerfen.

### 7.2 Betriebspunkt-Automat

```
        ┌──────────┐
        │  ANCHOR  │ ◄── Boot, TTL-Ablauf, Watchdog, Not-Aus-Erholung
        └────┬─────┘
             │ neuer Punkt angefordert
        ┌────▼─────┐
        │  RAMP    │ V/f in der richtigen Reihenfolge, PLL an Job-Grenze
        └────┬─────┘
        ┌────▼─────┐
        │  SETTLE  │ Messungen verworfen, bis Temperatur τ-stabil
        └────┬─────┘
        ┌────▼─────┐
        │  JUDGE   │ bewertet nach N Shares ODER T Sekunden — was zuerst kommt
        └──┬────┬──┘
     gut   │    │  schlecht
        ┌──▼─┐ ┌▼──────────┐
        │ HOLD│ │ FALLBACK │ → ANCHOR, Punkt für 1 h gesperrt
        └──┬──┘ └──────────┘
           │ anchor angefordert
        ┌──▼─────┐
        │ ANCHOR │
        └────────┘
```

**JUDGE bewertet in Shares, nicht in Sekunden.** Bei 1,2 TH/s und Pool-Diff
`d` dauert ein Share im Mittel `d × 2^32 / 1.2e12` Sekunden. Eine feste
60-Sekunden-Bewertung ist bei hoher Diff statistisch wertlos. Der Automat wartet
auf `n_min` Shares (Default 30 → relativer Fehler ~18 %) mit einem Zeitdeckel als
Notausgang. *(„VarDiff misst man in Shares" — gilt hier genauso.)*

**Punktwechsel-Erkennung:** Wenn jemand außerhalb (UI, andere API, anderes Tool)
Frequenz oder Spannung ändert, wird SETTLE/JUDGE neu aufgesetzt. Ein Punkt, der
halb unter alten und halb unter neuen Bedingungen bewertet wurde, ist eine Lüge.
(Das ist der Fix, den du in WarpTune schon gebaut hast — er gehört in beide
Schichten.)

### 7.3 Hashrate: zwei Zahlen, getrennt gemeldet

| Feld | Bedeutung |
|---|---|
| `hashrate_nominal_ghs` | `f_real × small_cores / 1000` — was der Chip *könnte* |
| `hashrate_effective_ghs` | aus akzeptierten Shares, EWMA über ein **Share-Fenster** |
| `hashrate_confidence` | 0..1, aus der Share-Zahl im Fenster |

ESP-Miner zeigt nur die erste und nennt sie „hashRate". Sie bleibt hoch, während
der Chip 40 % Müll produziert. Die dritte Zahl ist der Punkt, den heute alle
Tools raten müssen: *Wie sicher ist diese Messung überhaupt?*

### 7.4 Thermik

Vorsteuerung aus einem RC-Modell (`τ` pro Board im Profil) plus PI auf der
Junction-Temperatur. Konkret:

- **Plausibilitätsprüfung** zwischen ASIC-Diode und Board-NTC. Differenz
  außerhalb des Erwartungsfensters → beide als unzuverlässig markieren und
  konservativ fahren. Nicht: „0 °C, alles gut, Vollgas."
- **Drosselung weich und früh**: kleine Frequenzschritte ab `target - 3 K`,
  nicht ein harter Sprung bei `overheat`.
- **Lüfter mit Akustikdeckel**: eine konfigurierbare Obergrenze, die nur bei
  echter Not überschritten wird. Die Dinger stehen bei dir zu Hause.
- **Not-Aus dreistufig**: Frequenz → Spannung → Stop. Jede Stufe protokolliert.

### 7.5 Fehlerattribution pro Chip

Der Nonce trägt die Chip-Adresse. Also: HW-Error-Zähler, Nonce-Rate und
Desync-Rate **pro Chip**, nicht pro Board.

Bei n=1 (Gamma) ist das Buchhaltung. Bei n=4/8 (NerdQAxe++, Octaxe) ist es der
Unterschied zwischen „Board hat 3 % Errors" und „Chip 3 ist der Ausreißer, die
anderen sind sauber". Deshalb wird es in Stufe 1 gebaut, auch wenn es dort noch
nichts bringt — nachrüsten heißt das Datenmodell umbauen.

---

## 8. Betrieb

### 8.1 Konfiguration

NVS mit **Schema-Version** und atomarem Commit: Die gesamte Konfiguration wird
als eine Transaktion geschrieben, nicht Feld für Feld. Ein Stromausfall mitten
im Speichern darf keinen halben Zustand hinterlassen.

Migration beim Boot: `schema_v` lesen → Migrationskette anwenden → zurückschreiben.
Kein „das Feld gab es früher nicht, also ist es 0".

### 8.2 OTA, die nicht brickt

```
1. Upload in freien Slot          (A/B)
2. Signaturprüfung                (ed25519 über das Image)
3. Boot in neuen Slot, Flag PENDING
4. Health-Gate:
     - WiFi verbunden             innerhalb 60 s
     - Pool verbunden             innerhalb 120 s
     - ASIC initialisiert         innerhalb 30 s
     - ≥1 akzeptierter Share      innerhalb 600 s
     - kein Panic, kein WDT-Reset
5. alle erfüllt → Slot als VALID markieren
   sonst        → Reboot in alten Slot, Grund im Post-Mortem-Log
```

Das ist der Unterschied zwischen „Update hat was kaputtgemacht, ich fahre
zurück" und „Gerät ist tot, hol den USB-Adapter".

Secure Boot v2 und Flash-Encryption: vorbereitet, aber **standardmäßig aus**.
Einmal eingebrannt ist irreversibel, und ein Gerät, das man nicht mehr
umflashen kann, ist für ein Bastelprojekt das falsche Risiko.

### 8.3 Post-Mortem-Log

Ringpuffer im **RTC-RAM** (überlebt Soft-Reset und Deep-Sleep), gespiegelt nach
NVS bei sauberem Shutdown:

```
[reset_reason] [uptime_s] [freq_mhz] [core_mv] [junction_c] [vr_c]
[last_asic_evt] [desync_count] [hw_err_10min] [heap_free_min] [task_wm_min]
```

Nach jedem unerwarteten Reboot steht in der UI **warum**. Heute rätselst du.

### 8.4 Uhr

NTP mit mehreren Servern, und — wichtig — die Firmware rollt `ntime` nur im
Fenster, das der Pool erlaubt, gemessen an der **Pool-Zeit aus dem
Stratum-Job**, nicht an der lokalen Uhr. Eine falsch gestellte lokale Uhr darf
nicht zu systematisch abgelehnten Shares führen.

---

## 9. API-Fläche

### 9.1 Nativ

```
GET  /api/v1/state          vollständiger Zustand, ein Objekt
GET  /api/v1/stream         WebSocket, Deltas, ~1 Hz
POST /api/v1/operating-point  §7.1
GET  /api/v1/history        Ringpuffer letzte 24 h, 1-Minuten-Auflösung
GET  /api/v1/postmortem     §8.3
GET  /metrics               Prometheus
POST /api/v1/ota            signiertes Image
```

Versioniert ab Tag 1. `/api/v1` bricht nie; Neues kommt unter `/api/v2`.

### 9.2 AxeOS-Kompatibilitätsschicht — nicht verhandelbar

```
GET   /api/system/info      → aus /api/v1/state gemappt
PATCH /api/system           → frequency, coreVoltage → operating-point
POST  /api/system/restart
GET   /api/swarm/info
```

**Warum Pflicht:** WarpTune (`crates/tune/src/axeos.rs`), dein Dashboard, die
Swarm-Tools, jeder Community-Skript — alle sprechen AxeOS. Eine Firmware, die
das Ökosystem bricht, wird nicht benutzt, egal wie gut sie ist.

Die Kompatibilitätsschicht meldet zusätzlich `smallCoreCount`,
`sharesRejectedReasons` und (neu) `hashrateConfidence` — Felder, die WarpTune
bereits optional liest bzw. sofort nutzen könnte.

### 9.3 Authentifizierung

Optionaler Token auf allen schreibenden Endpunkten, Default **aus** (sonst
verlierst du jeden Bastler), aber im Setup-Wizard angeboten. Dass heute jeder im
LAN ungefragt die VCore verstellen kann, ist keine Entscheidung, sondern eine
Lücke.

### 9.4 Web-UI

Statisch, vorkomprimiert (brotli), Ziel < 120 KB, **keine externen Ressourcen**.
Keine Google-Fonts, kein CDN. Ein Miner im Keller darf nicht davon abhängen,
dass irgendwas aus dem Internet lädt.

i18n von Anfang an mit CI-Guard gegen hartcodierte Strings — dieselbe Regel wie
in WarpPool. Startsprachen: de, en. Weitere erst nach Muttersprachler-Review.

---

## 10. Teststrategie

### 10.1 Host (jeder Commit, Sekunden)

- Unit-Tests `forge-core`, `forge-proto`, `forge-control`
- **Property-Tests** (`proptest`): PLL-Löser (gelöste Frequenz immer innerhalb
  Toleranz & Constraints), Frame-Roundtrip (encode∘decode = id), Job→Work
  (Merkle-Root gegen unabhängige Referenzimplementierung)
- **Fuzzing** (`cargo-fuzz`) auf dem V1-Parser und dem `RxDecoder`. Ziel: kein
  Panic, kein Overflow, bei beliebigem Bytemüll.
- Golden Frames: gepinnte Bytefolgen aus echter Hardware, gegen die der Encoder
  geprüft wird

### 10.2 Simulator-Integration (jeder Commit, Minuten)

Die vollständige App-Logik gegen `forge-sim` im In-Process-Modus, mit
virtueller Zeit (24 h Betrieb in ~2 s). Jedes Szenario aus §6.3 als Testfall.

### 10.3 HIL (nightly + vor Release)

Ein Gamma an einem Runner. Flash → Boot → Pool (lokaler WarpPool auf regtest) →
mindestens ein akzeptierter Share → OTA-Zyklus → Rollback-Zyklus.

Regel aus der WarpPool-Historie, die hier genauso gilt: **Positiv-Beleg statt
Textmuster.** Der Test bestanden ist er, wenn ein Share *angekommen* ist, nicht
wenn „accepted" im Log steht.

### 10.4 Was NICHT getestet wird

Echte Hashrate-Zahlen im Simulator. Der Sim behauptet Nonce-Raten, er verdient
sie nicht. Alles, was „ist diese Frequenz wirklich schneller" beantwortet, geht
nur über HIL und Feld.

---

## 11. Meilensteine

| # | Inhalt | Aufwand* | Abnahmekriterium |
|---|---|---|---|
| ✅ M0 | *erledigt 2026-09-23:* Repo `~/code/dvb-WarpAxe`, Workspace, CI (fmt/clippy/test/no_std), GPLv3 | — | `cargo test` grün, `thumbv7em`-Build grün |
| ✅ M1 | *erledigt 2026-09-23:* `crc`, `frame`, `decoder` (Ein-Byte-Resync, Desync-Zähler). Fuzzing bewusst durch proptest ersetzt (kein Nightly). | — | 50 Tests grün, gepusht |
| ✅ M3 | *erledigt 2026-09-23:* `pll::solve` (integer, C-Orakel), `job`, `result`, `regs`, `chip::ChipProfile` + `InitStep` + `render()`, Profil `bm1370` als Daten. 80 Tests, Commit 05b0ccd. | — | Init-Sequenz reproduziert alle 12 kommentierten Wire-Frames in Reihenfolge |
| 🔶 M2 | *Kern erledigt 2026-09-23:* `cmd`-Decoder, `SimChip`/`SimChain` (Registerfile, Zustandsautomat, Poisson-Nonces, 7 Chip-Faults), 19 Sim-Tests; Chip-ID-Vorhersage fällt end-to-end aus dem Sim. Dazu `board` (Watt/Grad/VR/Lüfter, Gamma aus WarpTune-Messwerten) und `warpaxe-axeos`: AxeOS-API über dem Sim mit `/sim/fault`. **Offen:** WarpTune-Livetest, PTY, record/replay | — | Commit 53bfaa4, 127 Tests |
| **M0** | Repo, Toolchain, CI (host + xtensa build), `forge-core` Gerüst | 2–3 d | `cargo test` grün auf host, Image baut |
| **M1** | Frame-Layer + CRC + Golden Frames + Fuzzing | 3–4 d | Roundtrip-Property grün, Fuzzer 1 h ohne Fund |
| **M2** | `forge-sim` Grundfunktion (Init, Regs, Nonce-Erzeugung) | 4–5 d | Init-Sequenz aus esp-miner läuft gegen Sim durch |
| **M3** | Chip-Profil BM1370 + PLL-Löser | 3 d | Leiter generierbar, alle Punkte lösbar, ±0,5 % |
| **M4** | Stratum V1 (Parser, Job→Work, Submit) gegen Sim | 5–6 d | Sim „findet" Share → korrekter `mining.submit` |
| **M5** | **Erster echter Share auf Gamma-Hardware** | 3–5 d | akzeptierter Share auf WarpPool regtest |
| **M6** | Fehlerinjektion + Recovery-Pfade | 4–5 d | alle Szenarien §6.3 als Tests, keiner endet im Reboot |
| **M7** | Betriebspunkt-Automat + Thermik + Anker/TTL | 5–7 d | Sim: Punkt wird gehalten, TTL-Rückfall funktioniert |
| **M8** | API v1 + AxeOS-Kompat + WarpTune fährt das Gerät | 4–5 d | WarpTune tuned WarpForge ohne Codeänderung |
| **M9** | OTA + Health-Gate + Post-Mortem | 4–5 d | absichtlich kaputtes Image → automatischer Rollback |
| **M10** | Web-UI, i18n, Setup, Doku | 5–7 d | Gerät von null aufgesetzt ohne Terminal |

\* konzentrierte Arbeitstage, nicht Kalendertage. Summe M0–M5 (erster Share):
**~20–26 Tage**. Summe bis M10: **~45–55 Tage**.

Das ist ehrlich gerechnet und kein Wochenendprojekt. Der ehrlichste Satz dazu:
Bis M5 ist alles Lernen und Risiko, ab M6 wird es erst besser als der Bestand.

**Der Ausstiegspunkt mit dem besten Verhältnis ist M2+M3.** Siehe Teil 2.

---

## 12. Offene Entscheidungen

### 12.1 BM1366 / BM1368 — ENTSCHIEDEN: nein (2026-09-22)

**Oliver hat weder BM1366 noch BM1368 im Portfolio.** Mein ursprüngliches
Argument „der Bestand ist da, also testbar" war schlicht falsch — die Miner-Liste
enthält BM1370 (Bitaxe 602, BitForge Nano, NerdQAxe++, NerdOCTAXE-γ), BM1373
(NerdAxe Gaia) und den Avalon Q. Kein einziger der alten Chips.

Damit entfällt auch die abgeschwächte Empfehlung, die Registermaps als
unbestätigte Profile einzuchecken: Profile, die niemand je gegen Hardware prüfen
kann, sind keine Architekturprobe, sondern totes Gewicht mit einem
Wartungsversprechen dran.

**Konsequenz für das Design:** Die Behauptung „ein neuer Chip ist nur eine
Tabelle" muss anders belegt werden — nämlich am BM1373, sobald der Gaia als
zweites Profil dazukommt. Das ist der ehrlichere Test, weil beide Chips real
vorhanden sind und beide gemessen werden können.

### 12.2 Weitere offene Punkte

- **Sv2 in der Firmware**: Noise NX auf dem S3 kostet RAM (secp256k1-Kontext).
  Machbar, aber erst nach M8 bewerten. Kein Blocker für irgendwas.
- **Name**: „WarpForge" ist geraten. Passt ins Schema, kollidiert aber
  begrifflich mit Forge-OS (BitForge Nano), das WarpTune schon unterstützt.
  Alternativen: WarpAxe, WarpCore (belegt), WarpSilicon.
- **Lizenz**: AGPLv3 wie WarpPool? Bei Firmware ist AGPL schwach (keine
  Netzwerkinteraktion im Sinne der Klausel). GPLv3 wäre passender, und
  ESP-Miner-abgeleitete Registerdaten könnten ohnehin GPL-Pflichten auslösen —
  **vor M0 klären**, nicht danach.
- **Verhältnis zu ESP-Miner**: Eigenes Projekt oder irgendwann Upstream? Meine
  Einschätzung: eigenes Projekt. Die Architekturentscheidungen (Rust, Sim,
  Profil-als-Daten) sind nicht inkrementell einführbar.


---

## 13. Korrekturen und Befunde aus Stufe A (2026-09-23)

Beim Start von Stufe A wurde ESP-Miner `v2.15.2rc0-17-g0c798de` geklont und
`components/asic/` vollständig gelesen. Das Bild ist besser als in §3.1
beschrieben, und einige Behauptungen dieses Dokuments waren falsch.

### 13.1 Was ich falsch hatte

| Behauptung im Doc | Tatsächlich |
|---|---|
| §3.1 „`bm1366.c`, `bm1368.c`, `bm1370.c` — dreimal derselbe Code" | Es gibt inzwischen `asic_common.c` (Chip-Zählung, `receive_work`, Difficulty-Maske, Timeout-Rechnung), `pll.c` (gemeinsamer Löser) und `frequency_transition_bmXX.c`. Die Chip-Dateien halten nur noch Init-Sequenz, Response-Struct und Nonce-Layout. Die Kritik gilt abgeschwächt: die Init-Sequenzen *sind* noch Code, nicht Daten. |
| §7.1 „Rampen-Ordnung … ESP-Miner rampt nicht" | ESP-Miner rampt die **Frequenz** in 6,25-MHz-Schritten mit 100 ms Pause (`do_frequency_transition`). Was fehlt, ist die *Koordination mit der Spannung* — die wird separat im Power-Management gesetzt. Der offene Punkt bleibt, ist aber enger. |
| §3.3 „PLL: lösen statt Tabelle" als Neuerung | `pll_get_parameters()` **löst** bereits: refdiv 1–2, postdiv1/2 1–7, Constraint `postdiv1 > postdiv2`, fbdiv-Fenster pro Chip (BM1370: 160–239), Prioritäten *kleinste Abweichung → niedrigste VCO → kleinstes postdiv-Produkt*. Was fehlt: die *erreichte* Frequenz wird geloggt, aber nicht als API-Wert gemeldet. Der Rust-Löser wird gegen den C-Löser verifiziert, nicht neu erfunden. |
| §12.2 „BM1373 unbekannt" | `bm1373.c` existiert (552 Zeilen). Und der Test-Harness (`bm13xx_test_harness.c`) ist im Kern schon ein Mini-Simulator: fängt TX-Pakete ab, liefert gequeue'te Responses. Nur ohne Physik, ohne Fehlerinjektion, ohne Zeit. |

### 13.2 BM1373 ist ein anderes Wire-Format, kein Profil-Diff

Aus den Defines in `bm1373.c` (Register-Namen intern „BM1372"):

| | BM1370 | BM1373 |
|---|---|---|
| Chip-ID auf dem Bus | `0x1370` | `0x1372`, Alias `0x1373` |
| Chip-ID-Antwort | 11 Bytes | **9 Bytes** |
| Nonce-/Register-Antwort | 11 Bytes | 11 Bytes (gleiches Struct) |
| Max. Baud | 1 MBaud | **3 MBaud** |
| Adress-Stride | `256 / n` | **`0x10` fest**, Command-Address-Shift 2 |
| Schreib-Wiederholung | keine | 3× mit 50 ms |
| Init-Schritte | ~20 | mehr, mit Soft-Reset-Register `0xA8` = `0xF00701F0` |
| HCN | berechnet (`hcn_max − 2·134` Errata) | Konstante `0x1EB5` („S21 Pro") |
| PLL fbdiv | 160–239 | 160–239 (gleich) |
| Nonce-Layout | job\_id `(id&0xf0)>>1`, asic `nonce>>17`, core `nonce>>25 &0x7f`, small `id&0x0f` | **identisch** |

Konsequenz für `ChipProfile`: Es braucht neben den Registerwerten auch
`chip_id_reply_len`, `address_stride: Fixed(u8) | Divided`, `write_retries`
und `hcn: Computed | Fixed(u32)`. Das ist noch als Daten abbildbar — aber
das Profil-Format wird davon bestimmt, nicht nur vom BM1370.

**Core-Zahlen — recherchiert 2026-09-23, Ergebnis: keine der beiden Zahlen ist belegt.**

| Quelle | `small_cores` | Herkunft |
|---|---|---|
| ESP-Miner `device_config.h` | 6725 | PR #1890 (22.08.2026), **kein Wort zur Herleitung**, seither unverändert |
| NerdOS `bm1373.cpp` (BitMaker `develop`) | 6860 | `#define BM1373_SMALL_CORE_COUNT 6860` — **mit `TODO` markiert** |
| Naja-Duo-Werksmessung im PR #1890 | ~7000 | 4579 GH/s, 2 Chips, 327 MHz, 2022 Nonces (±2 %) |
| **Gaia, Chipzähler via NerdOS, 1 h bei 400 MHz** | **~6945** | `hashRate_1h` 2778 GH/s, nonce-basiert, ±2–3 % |

`small_cores` ist bei keinem BM13xx eine Strukturzahl (BM1366: 894 auf 112 Cores),
sondern ein *gemessener* Effektivwert. Beide Firmware-Zahlen sind Platzhalter.
Die Messung am Gaia favorisiert 6860 gegenüber 6725, kann 6860 von ~6950 aber
nicht trennen. **Auflösung auf < 1 %:** einen Tag bei fester Frequenz, dann
`hashRate_1d` — oder in Stufe B direkt `TOTAL_COUNT` (0x8C) über ein bekanntes
Intervall lesen, das zählt Hashes exakt. Der Profilwert für den BM1373 bekommt
das Attribut `#[src("measured Gaia 2026-xx-xx")]`, nicht eine der beiden Zahlen.

**Domains:** BitMaker (X, Gaia-Launch): *„Now all 8 domains are active!"* Der
BM1373 hat **8 Domains**; ESP-Miner konfiguriert `hash_domains = 4` und gleicht
mit `domain_hashrate_scale = 2.0` aus — liest also 4 Zähler und verdoppelt.
Das Profil braucht `domains = 8` und alle acht Zählerregister.

Für Stufe A (BM1370) ist das alles irrelevant; für WarpTune heute harmlos
(2 % zu niedrige Erwartung lässt das Gerät *besser* aussehen, kein Fehlurteil).

Beide Chips: `core_count = 128`, `hash_domains = 4`. BM1370 stock
525 MHz @ 1150 mV, BM1373 stock 327 MHz @ 1000 mV (ESP-Miner; NerdOS-Gaia: 350 @ 940).

### 13.3 Befund: die RX-CRC deckt die Flag-Bits ab

Beim Bau des Frame-Layers ist eine Annahme geplatzt und hat sich zu einer
Erkenntnis gewandelt. Das letzte Byte einer Chip-Antwort ist
`flags:3 | crc:5`. ESP-Miner prüft `crc5(alle 9 Bytes) == 0`. Diese
Eigenschaft gilt für einen CRC nur, wenn **nichts** zwischen den Daten und
den CRC-Bits steht — die 3 Flag-Bits müssen also Teil dessen sein, worüber
der Chip seinen CRC rechnet. Für TX-Frames (letztes Byte `000 | crc:5`)
gilt die Eigenschaft *nicht*, weil dort drei Null-Bits dazwischen stehen.

Belegt am einzigen RX-Vektor aus einem `bm1370.c`-Kommentar
(`AA 55 13 68 00 00 00 00 00 00 0F`, die Chip-ID-Antwort eines BM1368):

```
crc5(8 Bytes)                = 0x0C   ← nicht der Wire-Wert
crc5(8 Bytes + 3 Flag-Bits)  = 0x0F   ← der Wire-Wert
crc5(alle 9 Bytes)           = 0x00   ← ESP-Miners Prüfung geht auf
```

Das ergibt eine **überprüfbare Vorhersage** für den Gamma:

```
Read Register 0x00  →  AA 55 13 70 00 00 00 00 00 00 10
```

Das ist der erste Test, der in Stufe B am echten Gerät zu laufen hat. Stimmt
das letzte Byte nicht, ist die Flag-Bit-Regel falsch und `verify5` mit ihr.

Im Code: `crc::response_tag(payload, flags)` baut den Wert, den der
Simulator ins letzte Byte schreibt; `crc::verify5` prüft empfangene
Antworten; `frame::encode_response` / `frame::decode_response` sind das
Paar, das Simulator und Firmware verbindet. Ein proptest pinnt, dass jede
so gebaute Antwort verifiziert.

### 13.4 Namens- und Lizenzentscheid

Repo: `~/code/dvb-WarpAxe`, Crates `warpaxe-core` und `warpaxe-sim`.
**GPL-3.0-or-later**, weil Init-Sequenzen, Registerwerte und Test-Vektoren
wörtlich aus ESP-Miner (GPLv3) stammen. „WarpForge" ist damit nur noch der
Name dieses Notizordners.

### 13.5 PLL-Löser: Zahlen statt Behauptungen (2026-09-23)

`pll::solve` ist fertig und gegen einen `f32`-Port von `pll_get_parameters()`
auf jedem Viertel-MHz von 50 bis 1200 in beiden Divider-Fenstern geprüft —
identische Lösung, Ties eingeschlossen. Ergebnisse, die für WarpTune
unmittelbar relevant sind:

| Ziel | fb | ref | pd1/pd2 | tatsächlich | Register 0x08 |
|---|---|---|---|---|---|
| 525 (Gamma Stock) | 168 | 2 | 4/1 | 525,000 | `40 A8 02 30` |
| 600 | 192 | 2 | 4/1 | 600,000 | `50 C0 02 30` (VCO = 2400 → Scale-Bit) |
| **640 (Gamma hand-tuned)** | 205 | 2 | 4/1 | **640,625** | `50 CD 02 30` |
| 690 | 221 | 2 | 4/1 | 690,625 | `50 DD 02 30` |

**496 von 601 ganzzahligen MHz-Zielen zwischen 100 und 700 liegen nicht auf
dem PLL-Raster.** Der Gamma läuft seit Wochen auf 640,625 MHz, nicht auf
640 — jede J/TH-Rechnung in `ladder.rs`, die 640 annimmt, ist um 0,1 %
daneben. Klein, aber systematisch, und ein Argument dafür, dass die
Firmware die *erreichte* Frequenz melden muss (`frequencyActual` aus §9.2).

**BM1373-Auffälligkeit** (gleiches fb-Fenster 160–239): Zwischen 320 und
380 MHz zwingt das Fenster den Löser auf `refdiv = 1` mit VCO 4,8–5,7 GHz,
während 300/350/400 MHz bei ~2,1 GHz liegen. Ob das Silizium die hohe VCO
verträgt, ist unbekannt. Es passt aber dazu, dass NerdOS 350 als Stock
nimmt (VCO 2,1 GHz) und ESP-Miners 327 als 326,786 aufgelöst wird. Frage
für Stufe 2.

### 13.6 Der Simulator trifft WarpTune (2026-09-23, spät)

`warpaxe-axeos` serviert einen Bitaxe Gamma über HTTP aus `SimChain` +
`Board`. WarpTunes `probe` liest ihn als *„BM1370 board 602 Gamma, 1128 GH/s
of 1071 expected, 15,5 J/TH"* — ohne eine Zeile Änderung an WarpTune.

**Erster Live-Lauf: die Leiter rührte sich nicht.** Heartbeat:
`up blocked: VIN 5.00 V < 5.01`. Das Board-Modell meldete exakt die
Nominalspannung; WarpTunes VIN-Guard (existiert, weil ein zum Nominalwert
absackendes Netzteil ein Warnzeichen ist) hielt den Simulator für ein
defektes Netzteil — korrekt. Fix: PSU-Überhöhung 0,15 V, Sag 12 mV/A
→ 5,11 V unter Last (Commit `8dd1370`).

Das ist der Simulator-Zweck in Gegenrichtung: **Ein Regler mit echten
Guards findet die Stellen, an denen das Modell zu ideal ist.** Jede
solche Stelle ist ein Parameter, den man am echten Gerät messen sollte.

**Lehre zur Zeit:** `settle_s` (45), `judge_after_s` (600) und
`startup_s` (60) sind in WarpTune nicht konfigurierbar. Ein Leiterschritt
braucht also ≥ 10 Minuten Wandzeit, egal wie schnell der Simulator seine
Physik rechnet (`--time-scale` beschleunigt nur das Gerät). Für
Regressionstests, die in Sekunden laufen sollen, bräuchte WarpTune einen
Testhaken, der die Limits setzt — oder der Regulator wird direkt gegen
`Device::tick()` getrieben, ohne HTTP und ohne Wanduhr. Letzteres ist die
saubere Variante: `regulator.rs` ist „pure decision logic, no I/O", und
`Device` liefert `Sample`-kompatible Zahlen. Das wäre der nächste
sinnvolle Schritt auf der WarpTune-Seite.

### 13.7 Der Beweis (2026-09-23, 08:38–08:44)

WarpTune live gegen `warpaxe-axeos`, Sim auf 600 MHz, VR-Tod darüber
(Beleg: `02-Demo-WarpTune-vs-Sim-2026-09-23.txt`):

- **Power-Collapse-Guard** feuert 20 s nach dem Schritt auf 625, *innerhalb*
  des Settle-Fensters — die Lücke vom 22.09. ist geschlossen.
- **Dead-Chip-Guard** erkennt `coreVoltageActual 0` über drei Ticks und
  löst den Restart aus; der Sim kommt wieder hoch.
- **Fund:** Das Step-Memory behandelt den Kollaps-Punkt als Loch, nicht als
  Decke — nach dem Restart „skipping 625 (unstable); up 600→650", zweiter
  Kollaps, zweiter Restart. Auf Hardware: zwei VR-Tode, bevor der Regler
  aufgibt. Vorschlag an die WarpPool-Session: Collapse/Dead-Chip bei X →
  alle Schritte ≥ X für die Hold-Dauer sperren.

Damit ist die Kernbehauptung von `01-Nutzen-fuer-WarpTune.md` eingelöst:
Der Simulator ist ein Testgerät, gegen das der Regulator laufen kann, das
sich falsch verhält, und das Fehler findet, die im Feld Hardware kosten.

### 13.8 Die Regressionssuite (2026-09-23, Commit 9278ebf)

Auf Olivers Go: `crates/scenarios/` in WarpAxe treibt WarpTunes
`Regulator` **in-process** gegen `Device` — kein HTTP, keine Wanduhr,
`Limits` per Struct-Literal verkürzt. Fünf Feldfehler als Tests, **40 ms**
für alle: gesunder Aufstieg; VR-Tod im Settle-Fenster mit Decke (pinnt
dvb-WarpPool 1c80963); Chip-Hang mit Leistung; Sensor auf 0 → keine
Writes; Duplikat-Nonces blockieren. Paket ist vom Workspace abgekoppelt
(Path-Dep auf dvb-WarpPool); Lauf: `cargo test --manifest-path
crates/scenarios/Cargo.toml`. Sobald dvb-WarpPool die Decke taggt: git-Dep
mit festem `rev` + eigener CI-Job.

Der Live-Rerun mit `--seed 5` gegen das neue Binary bestätigte den Fix
unabhängig davon über HTTP (ein Restart, keine zweite Kletterei; Beleg im
Demo-File).

**Damit ist Stufe A über das Ziel hinaus erfüllt.** Was das Entscheidungs-Doc
als Stufe-A-Ertrag versprach — „der Simulator als Testgerät für den
Regulator" — existiert, hat am ersten Tag einen Reglerfehler gefunden, und
der Fehler ist als Test festgenagelt, bevor er gefixt gemerged wurde.

### 13.9 Der Avalon-Block (2026-09-23, auf Olivers Go)

`warpaxe-sim::avalon::AvalonQ` modelliert den Q als MM-Controller (kein
Chip-Sim): drei Work-Modes mit den Messwerten vom 11.09., Moduswechsel =
Hashboard-Reinit (55 W Standby, `GHSspd` 0 für 120 s, 60 s Rampe), echtes
900-s-Fenster für `MHS 15m`, `STATE[1]` immer, kein `coreVoltageActual`.
`warpaxe-cgminer` spricht das cgminer-Protokoll auf TCP 4028; WarpTunes
eigener `AvalonClient` liest und schreibt den emulierten Q **unverändert**
(Test grün).

Die Szenarien mit den *echten* Avalon-Limits (settle 180, judge 900,
collapse-hold 300) lieferten drei Funde — der Pfad, den WarpTune „noch nie
live gesehen" hatte:

| | Fund | Vorschlag |
|---|---|---|
| **A** | Gesunder Standard→Super-Wechsel wird bei 900 s mit 86 % verurteilt, weil `MHS 15m` den ~180-s-Reinit enthält → urgent down → nächster Reinit → Eco. **Kaskade auf gesundem Gerät.** | `judge_after_s = settle_s + 900` für Avalon: Fenster liegt dann hinter dem Reinit |
| **B** | Kollaps wird erkannt (Dead-Chip-Hashrate-Zweig +270 s, vor dem 300-s-Power-Hold), Restore belebt — dann A. | mit A erledigt |
| **C** | Verweigertes `ascset` wird alle 240 s endlos wiederholt, Stufe nie markiert. | nach 2–3 Fehlverifikationen markieren wie instabil |

Die drei Szenarien liegen als `#[ignore]`-Tests mit dem Fund als
Begründung vor; sie werden scharf, sobald WarpTune die Punkte ändert.
Modellannahmen, die man anzweifeln darf: Reinit 120 + Rampe 60 s
(„1–3 min" laut WarpPool-Session), Standby 55 W.

**Nachtrag, selber Abend:** A und B sind in WarpTune gefixt (`83faac4`:
Share-Zähler als zweite Stimme beim dringlichen Abstieg, Share-Fenster ab
`settle_s`, Avalon-Judge bei `settle + 900`) und live bei Oliver. Das Modell
wurde auf cgminers echtes Verhalten umgestellt — `MHS 15m` ist ein
exponentiell abklingender Mittelwert (τ 900 s), kein Fenster — was
Szenario A *strenger* macht; der Fix hält trotzdem. C war ein Artefakt
meines Harness: Der echte Runner schaltet nach 12 unverifizierten Ticks
die Live-Steuerung ab (`live_off`) und schreibt nie ein zweites Mal; nach
dem Nachbau dieses Pfads bestätigt der Test genau das. **Alle 14 Szenarien
grün**, keine `#[ignore]` mehr. Offen (Olivers Entscheidung): ob `live_off`
bei einem verweigerten `ascset` zu hart ist.

## 14. Transport: USB-C und LAN neben WLAN (Wunsch von Oliver, 2026-09-23)

**ENTSCHIEDEN (Oliver, 2026-09-23): Standard ist die USB-Bridge (14.6, Weg 2).**
Der Miner ist USB-NCM-Gerät; ein Linux-Host daneben brückt USB auf LAN.
LAN direkt am Miner (W5500) und Miner-als-USB-Host (14.6, Weg 1) sind
Optionen, kein Standard.

**Frage:** Kann WarpAxe die USB-C-Buchse der Bitaxe-/NerdAxe-Hardware als
Datenschnittstelle nutzen, und geht LAN?

### 14.1 Was die Hardware hergibt

| Board | MCU | USB-C | LAN |
|---|---|---|---|
| Bitaxe Gamma 601/602 | ESP32-S3-WROOM-1 | ja, D+/D- direkt an GPIO20/GPIO19 (kein Bridge-Chip, keine Serienwiderstände, kein ESD-Schutz) — **im Schaltplan geprüft, s. 14.5** | nein |
| NerdQAxe+/++ | ESP32-S3 | ja, wie oben | nein |
| NerdAxe Gaia | ESP32-S3 (Bitronics nennt „ESP-Miner-Version“, 2.4-GHz-WLAN only) | ja | nein |

Der ESP32-S3 hat **einen** USB-Full-Speed-PHY (12 Mbit/s), der per Mux
entweder am `USB_SERIAL_JTAG` (Werkszustand: Flashen, Konsole) oder am
`USB_OTG`-Controller hängt. Einen Ethernet-MAC hat der S3 **nicht**
(anders als der alte ESP32). LAN heißt darum immer: externer Chip per SPI
(W5500, KSZ8851SNL, DM9051 — alle mit `esp_eth`-Treiber im IDF) und damit
eine **Board-Revision oder ein Hat**. Die aktuellen Boards haben dafür
keinen Header; nur Testpads.

### 14.2 USB-C als Netzwerk: CDC-NCM

Das IDF liefert `esp_tinyusb` mit den Klassen CDC-ACM, MSC, HID, DFU,
**ECM/NCM/RNDIS**. Mit NCM erscheint der Miner am Host als
Netzwerkkarte: macOS und Linux nativ, Windows ab 11 ohne Treiber, ältere
Windows über RNDIS. Das IDF-Beispiel `examples/peripherals/usb/device/tusb_ncm`
(S2/S3/P4) macht genau das, nur andersherum (ESP als WLAN-Dongle). Wir
drehen es um:

```
[Host]  usb0: 192.168.7.1/30  ←USB-C→  [WarpAxe] usbnet: 192.168.7.2
```

- Der Miner betreibt auf dem USB-Netif einen **DHCP-Server** (wie der
  Wi-Fi-AP-Modus), Adresse aus einem link-lokalen /30 oder /24, damit
  mehrere Miner an einem Hub nicht kollidieren (Präfix aus der MAC).
- **AxeOS-API, Web-UI und Stratum laufen unverändert** — es ist nur ein
  weiteres `esp_netif`. WarpTune sieht `http://192.168.7.2/api/system/info`.
  Keine Sonderpfade, kein zweites Protokoll.
- **Stratum über USB:** liegt der Pool auf dem Host (WarpPool auf Mac,
  Umbrel, Hetzner-Gateway), erreicht der Miner ihn über die USB-Adresse
  direkt — **ohne WLAN, ohne NAT**. Pool im Internet → Host muss
  forwarden (macOS Internet-Sharing, Linux `ip_forward` + NAT); das ist
  Host-Konfiguration, nicht unsere.
- **Fallback-Priorität:** USB-Netif, wenn Link da; sonst WLAN. Beide
  gleichzeitig ist erlaubt; Stratum nimmt die Route mit dem Pool.
- **Kosten:** NCM belegt den OTG-Controller. `USB_SERIAL_JTAG` ist dann
  im Betrieb weg: Konsole läuft über CDC-ACM als zweites Interface
  (Composite NCM + ACM ist bei TinyUSB Standard), **Flashen bleibt**,
  weil der ROM-Bootloader im Download-Modus (BOOT gedrückt) den PHY
  selbst zurückmuxt. Das ist zu testen, bevor es Gerät Nr. 1 trifft —
  ein Brick-Pfad wäre hier fatal. Bis dahin: Kconfig-Schalter,
  Default WLAN + Serial-JTAG wie heute.
- **Strom:** VBUS ist am Gamma **nicht angeschlossen** (s. 14.5). USB-C
  liefert dem Board keinerlei Energie, auch nicht der Logik; ohne
  Netzteil am Hohlstecker ist der ESP32 tot, auch zum Flashen. Umgekehrt
  kann der Miner den Host-Port nie belasten. Für WarpAxe heißt das: der
  USB-Link ist reine Datenstrecke, kein Power-Zustand, den die Firmware
  unterscheiden müsste.

### 14.3 Die schlichte Variante: CDC-ACM mit Zeilenprotokoll

Wenn NCM zu viel ist: Serial-JTAG bleibt, wie es ist, und WarpAxe spricht
auf der Konsole ein **JSON-Lines-Protokoll** (`{"cmd":"info"}` →
`{"hashrate":…}`), dasselbe Vokabular wie die native API aus §9.1. Host-
seitig braucht das ein Tool oder einen WarpTune-Transport für
`/dev/tty.usbmodem*`. Vorteil: null Risiko am USB-Stack, funktioniert
sofort. Nachteil: WarpTune braucht einen zweiten Transport, Stratum geht
nicht darüber. **Das ist der Notnagel, nicht das Ziel.**

### 14.4 Was es für WarpAxe bedeutet

1. **Architektur jetzt:** die Netzwerkschicht ist `esp_netif`-agnostisch.
   Kein Code darf „WLAN“ annehmen — weder der Not-Aus (§7, „darf nicht
   von WLAN abhängen“) noch die Reconnect-Logik noch die UI-Anzeige
   „Signalstärke“. Netifs sind eine Liste: `wifi`, `usb`, später `eth`.
2. **Stufe B (Firmware, BM1370):** WLAN + NCM/ACM-Composite als Kconfig-
   Option, Default aus. Erster HIL-Test: Flashen nach NCM-Betrieb.
3. **LAN:** erst, wenn es ein Board mit W5500 gibt. Softwareseitig ist es
   dann ein weiterer `esp_netif` (`esp_eth` + SPI), ~1 Tag.
4. **Simulator:** die Szenarien brauchen nichts Neues — der AxeOS-Emulator
   ist bereits „nur eine IP“. Ein PTY-Transport für die ACM-Variante
   (§13.9-Restliste) würde §14.3 host-testbar machen.

**Entschieden:** NCM (14.2) ist das Ziel, ACM (14.3) nur wenn der
Flash-Rückweg auf Hardware nicht sauber ist. LAN am Miner bleibt offen,
bis ein Board es hat; der Standardweg ins LAN ist die USB-Bridge (14.6).

### 14.5 Schaltplan-Prüfung Bitaxe Gamma (2026-09-23, `bitaxeorg/bitaxeGamma` main, `esp32.kicad_sch`)

Netzliste aus dem KiCad-Quelltext extrahiert (eigener S-Expression-Parser,
Pins über Symbol-Transformation, Wire-Segmente inkl. T-Abzweige geprüft;
die `no_connect`-Marker lagen exakt auf den berechneten Pin-Positionen,
was die Transformation validiert):

| J5 USB-C-Pin | Netz |
|---|---|
| A6, B6 D+ | → U4 Pin 14 `GPIO20/USB_D+`, direkt, nichts dazwischen |
| A7, B7 D- | → U4 Pin 13 `GPIO19/USB_D-`, direkt |
| A4, B4 VBUS | **no_connect** |
| A5 CC1, B5 CC2 | **offen** — keine 5,1-kΩ-Pulldowns auf dem Board |
| A8, B8 SBU | no_connect |
| A1, B1, S1 | GND + Shield |

Auf dem ESP32-Blatt gibt es nur einen einzigen Widerstand (R16 10k, EN-Pullup),
keine Dioden, keine ESD-Bausteine, keinen USB-UART-Chip. Der 3V3-LDO
(RT9080) hängt am 5V-Netz des Netzteils, nicht an VBUS.

**Folgen:**

1. **Native USB bestätigt.** CDC-NCM/ACM über `USB_OTG` ist hardwareseitig
   möglich; nichts auf dem Board steht dem im Weg.
2. **Keine CC-Pulldowns → C-zu-C-Kabel unzuverlässig.** Ein echter USB-C-
   Host-Port erkennt ohne Rd kein Gerät und schaltet D+/D- meist gar nicht
   durch. Für den Dauerbetrieb als Datenstrecke also: **USB-A-zu-C-Kabel**
   oder ein Hub mit A-Ports. Das gehört in die Doku, sonst „geht nicht“.
3. **Kein ESD-Schutz auf D+/D-.** Für eine ständig gesteckte Verbindung
   im Betrieb ein Hinweis, kein Blocker; der S3 hat internen Schutz.
4. **VBUS unbeschaltet** — siehe Strom-Absatz in 14.2.

NerdQAxe/Gaia (Pmaxsd) sind nicht mit Quellen geprüft; die Boards sind
geschlossen (nur Gerber/Firmware öffentlich). Annahme bleibt „gleiche
S3-Standardbeschaltung“, bis ein Gerät am Tisch liegt.

### 14.6 USB ↔ LAN rein in Software? (Frage Oliver, 2026-09-23)

Ja, zwei Wege — einer auf dem Miner, einer auf einem Host daneben.

**Weg 1: Miner als USB-Host, USB-Ethernet-Adapter am USB-C.**
Der ESP32-S3 kann USB-Host (OTG). Espressif liefert in `esp-iot-solution`
die Komponenten `iot_usbh_ecm` und `iot_usbh_rndis` (S2/S3/P4), die ein
CDC-ECM- bzw. RNDIS-Gerät als `esp_netif` einhängen — gebaut für
4G-Sticks, funktioniert aber mit jedem Adapter, der die Klasse spricht.
Für WarpAxe wäre das ein weiteres Netif, wie in 14.4 vorgesehen. Haken:

- **VBUS ist am Gamma nicht angeschlossen (14.5).** Der Miner kann den
  Adapter nicht versorgen. Es braucht einen Adapter mit eigener
  Stromeinspeisung (Bauart „OTG-Ethernet mit Power-In“ für Chromecast /
  Pi Zero) oder einen fremdversorgten Hub. Normale Adapter bleiben tot.
- **Der Adapter-Chip muss ECM/NCM/RNDIS können.** RTL8153-basierte
  Adapter haben eine ECM-Konfiguration; ASIX AX88772 (u. a. Apples
  Adapter) spricht nur Vendor-Protokoll und fällt raus. Das heißt: eine
  kurze Positivliste getesteter Adapter, sonst Support-Hölle.
- Host-Stack kostet RAM auf dem S3 und belegt denselben PHY → Serial-JTAG
  weg, wie bei NCM-Device.

Ergebnis: geht, ist aber eine Bastelstrecke mit Hardware-Abhängigkeiten,
die wir nicht kontrollieren.

**Weg 2: Miner als USB-NCM-Gerät (14.2), ein Linux-Host brückt auf LAN.**
Pi Zero 2 / Pi 4 / Umbrel-Box als USB-Host, `usb0` und `eth0` in einer
Layer-2-Bridge (`systemd-networkd`: ein `Bridge=`-Eintrag, kein NAT,
kein Routing). Der Miner bekommt seine IP vom Heim-DHCP wie jedes
LAN-Gerät, WLAN aus, WarpTune und der Pool sehen ihn genau wie heute.
Mehrere Miner an einem Hub, jeder ein `usbN`, alle in derselben Bridge.
Kabel: USB-A-zu-C wegen der fehlenden CC-Pulldowns (14.5).

- Miner-seitig **null Zusatzcode** gegenüber 14.2.
- Host-seitig Standard-Linux, ~10 Zeilen Konfiguration; Doku-Aufgabe.
- Auf dem Mac: Internet-Sharing oder eine Bridge über `ifconfig bridge`
  — geht, ist aber nichts für Dauerbetrieb.

**Entschieden (Oliver, 2026-09-23): Weg 2 ist der Standard („USB-Bridge“).**
Weg 1 wird Option hinter einem Kconfig-Schalter, erst wenn ein bestimmter
Adapter mit Power-In als tauglich vermessen ist. Beides braucht 14.2
als Unterbau; nichts davon ändert die Architekturentscheidung aus 14.4.

Daraus folgt für die Meilensteine:

- **Stufe B (Firmware BM1370):** NCM-Device + CDC-ACM-Konsole als
  Composite, Kconfig-Schalter, Default aus, bis der Flash-Rückweg auf
  Hardware belegt ist. Danach Default an.
- **Doku „USB-Bridge“:** Rezept für Pi Zero 2 / Pi 4 / Umbrel-Box
  (`systemd-networkd`: `br0` mit `eth0` + `usb*`, Miner-Seite DHCP-Client
  auf dem USB-Netif), Hub-Betrieb mit mehreren Minern, Kabelhinweis
  USB-A-zu-C (14.5). Gehört zum Release von Stufe B, nicht danach.
- **HIL-Testfall:** Miner ohne WLAN-Zugangsdaten, nur USB an einer
  Bridge, muss booten, DHCP holen, Pool erreichen und per AxeOS-API
  antworten. Das ist der Abnahmetest für den Transport.
