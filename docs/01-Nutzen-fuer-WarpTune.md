# WarpForge ↔ WarpTune — was davon lohnt sich wirklich?

*Stand 2026-09-22. Entscheidungsvorlage, keine Bauanweisung.*

---

## Die unbequeme Vorbemerkung

WarpTune hat die Regelungsintelligenz **schon**: `regulator.rs` (3220 Zeilen,
Leiter-Controller mit Hysterese, Dwell, Guards, Step-Memory),
`ladder.rs` (Vendor-Tabellen inkl. BM1370, Spannungskurve, beyond-spec),
`journal.rs`, `pool.rs`, Punktwechsel-Erkennung, Energiemodus.

Eine eigene Firmware baut davon **nichts** neu auf, was du nicht hast. Sie
liefert nur Dinge, die das Sidecar *prinzipiell* nicht liefern kann. Die Frage
ist also nicht „bauen oder nicht", sondern: **welche vier, fünf Dinge sind das,
und was kosten sie einzeln?**

---

## Die Engpässe von WarpTune heute — und wer sie lösen kann

| # | Engpass | Lösbar im Sidecar? | Was eine Firmware ändern würde |
|---|---|---|---|
| 1 | **Hashrate ist eine Nominalzahl.** AxeOS meldet `frequency × smallCores`. Der Regulator muss aus `error_pct` und Reject-Strings rückschließen, ob der Punkt wirklich trägt. | ❌ nein | `hashrate_effective` aus Shares + `hashrate_confidence`. Der Regulator bekäme eine *gemessene* Zielgröße statt einer geschätzten. |
| 2 | **Reject-Semantik ist geraten.** `is_benign_reject()` klassifiziert nach Textmustern („stale", „low difficulty"). Jeder Pool formuliert anders. | ⚠️ nur näherungsweise | Lokale Share-Verifikation vor dem Submit → stale/ungültig wird gar nicht erst eingereicht, HW-Errors sind sauber getrennt. Reject-Raten würden aussagekräftig. |
| 3 | **Latenz der Regelschleife.** Poll → entscheiden → PATCH → 100 ms Anwendung → nächster Poll. Ein instabiler Punkt hasht Sekunden lang Müll, bevor jemand eingreift. | ❌ nein | Lokaler Automat reagiert in ms, Sidecar entscheidet weiter die *Strategie*. |
| 4 | **Rampen-Ordnung liegt bei der Firmware.** Korrektur: `patch_operating_point()` schickt `frequency` + `coreVoltage` in **einem** PATCH — das Sidecar macht es richtig. Aber welche der beiden die Firmware zuerst anwendet, kann es nicht vorgeben. | ❌ nein | Rampe explizit: V vor f hoch, f vor V runter, PLL-Wechsel an der Job-Grenze. **Siehe Vorfall unten — hier ist das keine Theorie.** |
| 5 | **Sidecar stumm = Gerät unbewacht.** Fällt der Mac-Dienst aus, bleibt der letzte Punkt stehen — ohne Absicherung. | ❌ nein | Anker + TTL: Gerät fällt eigenständig auf den letzten *validierten* Punkt zurück. |
| 6 | **Reboot-Ursachen unsichtbar.** Ein Gerät startet neu, das Journal sieht eine Lücke. Warum? | ❌ nein | Post-Mortem-Log über `/api/v1/postmortem`. |
| 7 | **Firmware-Unterschiede.** ESP-Miner / NerdOS / Forge-OS melden dieselbe Sache unter drei Namen (`temp2` vs `chiptemp1/2`). `axeos.rs` ist voller Fallbacks. | ⚠️ pflegbar | Löst nur *ein* Gerät. Die anderen bleiben. Kein echter Gewinn. |
| 8 | **Frequenz-Anzeige lügt.** Die PLL trifft nicht jede Wunschfrequenz; angezeigt wird die gewünschte. J/TH-Rechnungen sind entsprechend schief. | ❌ nein | PLL-Löser meldet die erreichte Frequenz. |

**Fazit der Tabelle:** 6 von 8 Engpässen sind nur in der Firmware lösbar. Das ist
ein starkes Argument — aber nur, wenn die Firmware auch läuft, und M0–M5 sind
~20–26 Tage, bevor der erste Share ankommt.

---

## Die drei Ausbaustufen, nach Nutzen pro Tag sortiert

### Stufe A: Simulator + `forge-core`, ohne Firmware (M1–M3, ~10–12 Tage)

**Was entsteht:** Eine Rust-Bibliothek, die BM1370-Frames spricht, die PLL löst
und die Registermap kennt — plus ein Host-Simulator mit Fehlerinjektion.

**Was das *sofort* bringt, ohne eine Zeile Firmware:**

1. **Der PLL-Löser korrigiert `ladder.rs` heute.** Deine Leiter ist ein
   25-MHz-Raster mit interpolierten Spannungen aus `device_config.h`. Mit dem
   Löser bekommst du die tatsächlich erreichbaren Punkte — und der Kommentar in
   `ladder.rs` („Upstream does not define the pairing; it is our reading of the
   two lists") wäre keine Vermutung mehr, sondern gerechnet.
2. **`forge-sim` als Testgerät für WarpTune.** Heute testest du den Regulator
   gegen Fixtures (`crates/tune/src/fixtures`). Mit dem Sim + einem dünnen
   AxeOS-Emulator davor testest du ihn gegen ein Gerät, das sich *falsch
   verhalten kann*: thermisch weglaufen, oberhalb X MHz hängen, Brownout bei
   niedriger Spannung. Das sind genau die Fälle, für die der Regulator gebaut
   ist — und die du heute nur erwischst, wenn sie im Feld passieren.
3. **Mehrere virtuelle Geräte.** Die Parallel-Session-Logik und die
   Energie-Attribution gegen 8 simulierte Miner testen, ohne 8 Geräte laufen zu
   lassen.

**Urteil: Das ist mit Abstand das beste Verhältnis.** Es verbessert das, was du
*heute in Produktion hast*, und es ist gleichzeitig die Grundlage für alles
Weitere. Wenn du nach Stufe A aufhörst, hast du trotzdem gewonnen.

### Stufe B: Firmware bis zum ersten Share + API (M4, M5, M8, ~12–16 Tage obendrauf)

Jetzt läuft ein Gamma auf eigener Firmware, WarpTune redet über die
AxeOS-Kompatibilitätsschicht mit ihm, und die Engpässe 1, 2, 4 und 8 sind weg.

**Voraussetzung:** ein Gamma, das du opfern kannst. Nicht der, der gerade im
7-Tage-Gamma-Test hängt.

**Urteil:** Lohnt sich, wenn Stufe A Spaß gemacht hat. Vorher nicht entscheiden.

### Stufe C: Der Rest (M6, M7, M9, M10, ~18–24 Tage)

Recovery-Pfade, lokaler Automat, OTA-Health-Gate, UI. Das ist der Teil, der aus
„funktioniert bei mir" ein Gerät macht, das man wochenlang unbeaufsichtigt
laufen lässt.

**Urteil:** Erst wenn Stufe B im Dauerbetrieb überzeugt. Und ehrlich: Das ist der
Punkt, an dem aus einem Lernprojekt ein Wartungsvertrag wird.

---

## Konkrete Kopplungspunkte zu WarpTune

Falls gebaut wird, diese Felder in der AxeOS-Kompat-Schicht **zusätzlich**
melden — `axeos.rs` liest optional und degradiert sauber, also kostet es dort
nichts, sie einzubauen:

| Feld | Nutzen im Regulator |
|---|---|
| `hashrateEffective` / `hashrateConfidence` | Ersetzt die Rückschlüsse aus `error_pct`; JUDGE kann auf Konfidenz warten statt auf Zeit |
| `frequencyActual` | Korrekte J/TH-Rechnung |
| `hwErrorsByChip[]` | Bei Mehrchip: Ausreißer statt Boardmittel |
| `desyncCount` | Frühindikator Signalintegrität |
| `lastResetReason` | Journal-Lücken erklären |
| `anchorPoint` | Sidecar sieht, worauf das Gerät zurückfällt |
| `operatingPointState` | RAMP/SETTLE/JUDGE/HOLD — Sidecar weiß, wann Messungen gültig sind |

Das letzte Feld ist das wertvollste: Heute muss WarpTune raten, wann die
Settle-Phase vorbei ist. Das Gerät weiß es.

---

## Empfehlung

1. **Stufe A bauen** (~10–12 Tage), aber als eigenständiges Projekt mit eigenem
   Wert: „BM1370-Protokollbibliothek + Simulator". Nicht als Firmware-Vorstufe
   framen — dann steht man unter Druck, weiterzumachen.
2. **`forge-sim` in die WarpTune-Tests hängen.** Das ist der Tag, an dem sich das
   bezahlt macht.
3. **Stufe B danach neu bewerten**, mit den Erfahrungen aus A und wenn die
   Gaia-Hardware (NerdAxe Gaia) da ist (dann weißt du auch, ob BM1373 ein Profil-Diff ist oder
   eine Überraschung).
4. **BM1366/1368**: Profile extrahieren, nicht verifizieren. Siehe Design-Doc
   §12.1.

**Was jetzt NICHT passiert:** Nichts davon wird gebaut, solange v1.32.0 nicht
deployed ist und der Gamma-Test nicht durch ist.

---

## Feldbeleg: der stille VR-Tod am Gaia, 2026-09-22 11:44 (09:44 UTC)

Am Tag der Inbetriebnahme, keine 12 Stunden nach dem Entwurf dieses Dokuments,
ist genau der Fehlerfall eingetreten, für den §6.3 im Design-Doc die
Fehlerinjektion vorsieht. Der Ablauf aus dem WarpTune-Journal:

```
09:44:46  decision  up 420→440 MHz, predicted 38.5 W of 40.0 W allowed
09:44:46  write     {"frequency":440,"coreVoltage":970}     ← ein PATCH, beides
09:44:51  write_verified  ok
09:45:16  heartbeat f=440  V=970  P=4.99 W  hr=2655  temp=48.6  vr=49.5
          "point changed 420→440 MHz @ 960→970 mV: settling 45s, judged after 600s"
   ...    (sechs Minuten nichts)
09:51:10  write     restore on shutdown          ← Oliver greift von Hand ein
```

30 Sekunden nach dem Schreiben: Leistung von **36 W auf 4,99 W**, Temperatur von
61 auf 48,6 °C, VR von 72 auf 49,5 °C. Der VR-Ausgang war auf 0, der Chip hat
nicht mehr gehasht.

**Was das Gerät dazu meldete:** `shutdown: false`. Kein Fault-Flag, kein
Overheat-Flag, kein Reboot. Und — der Punkt — **`hashrate_ghs: 2655`**. Die
gemeldete Hashrate blieb voll stehen, während das Gerät nichts mehr tat. Das ist
Engpass 1 dieser Tabelle, live vorgeführt: die Nominalzahl aus
`frequency × smallCores` überlebt den Tod des Chips.

Der einzige ehrliche Messwert im ganzen Datensatz war die **Leistung**.

### Drei Lehren

**1. Für WarpTune (sofort umsetzbar):** Der Regulator hat sechs Minuten nicht
reagiert, weil er nach einem Punktwechsel in `settling 45s, judged after 600s`
stand. Ein Sturz von 36 W auf 5 W braucht aber keine 600 Sekunden
Urteilsfindung — das ist ein Not-Aus-Signal, kein Messwert. Es fehlt ein
Watchdog, der **während** der Settle/Judge-Phase auf katastrophalen
Leistungseinbruch (z. B. < 30 % des vorhergesagten Werts) sofort auf den letzten
laufenden Punkt zurückfällt. Das ist unabhängig von jeder Firmware-Frage und
sollte gebaut werden, egal wie das Firmware-Projekt ausgeht.

**2. Für die Rampen-Ordnung:** Der Schritt hat Frequenz *und* Spannung
gleichzeitig geändert (420→440 MHz, 960→970 mV). WarpTune hat das korrekt als
einen PATCH geschickt — aber in welcher Reihenfolge NerdOS die beiden Werte
anwendet, ist nicht beeinflussbar. Wird die Frequenz vor der Spannung gesetzt,
läuft der Chip kurz mit 440 MHz auf 960 mV. Ob das die Ursache war, ist damit
nicht bewiesen — aber es ist die Sorte Unbestimmtheit, die eine eigene Firmware
beseitigt und eine API niemals.

**3. Für das Firmware-Design:** Der Fall gehört als Pflicht-Szenario in die
Fehlerinjektion — er ist realistischer als alles, was ich mir am Schreibtisch
ausgedacht habe:

```rust
Fault::SilentVrShutdown {
    above_mhz: 440,
    // VR-Ausgang auf 0, Leistung auf Standby-Niveau,
    // KEIN Fault-Flag, KEIN Reboot, Registerzugriff bleibt beantwortbar
}
```

Erwartetes Verhalten der Firmware: Der Leistungseinbruch wird innerhalb eines
Ticks erkannt, der Punkt gilt als schlecht, Rückfall auf den letzten Anker,
Eintrag im Post-Mortem-Log. Ohne Handeingriff, ohne Netzverbindung, ohne
Stromtrennung.

### Was Oliver getan hat

`ladder_cap_mhz = 400` — einen Schritt unter den letzten Punkt, der lief (420),
mit der Notiz *„raise only with a judged 400/420"*. Das ist die richtige
Reaktion: Der Gerätepunkt ist die validierte Obergrenze, und validiert ist nur,
was ein Urteil überstanden hat.
