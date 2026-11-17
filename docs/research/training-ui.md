# Training (practice) UI — UX research and recommendations

Status: research for phase 1 · Date: 2026-11-17 · Scope: practice/review UI only (not import, not word list browsing)

Revision 2 adds the product owner's decisions:

- server-side neural TTS for every word and sentence;
- the new **sentence comprehension** exercise built on sentences extracted from the book PDF;
- the **1–5 self-rating scale** (1 = best), used everywhere instead of Again/Hard/Good/Easy.

The new material is in sections 8–10 and is folded into the summary, the other sections, the wireframes and the open questions.

Revision 3 adds the **thin-app principle**: every language judgement comes from Claude in the import pack, and the app does only mechanical matching (section 11). Speech recognition is **out of scope**. Audio is playback only (TTS, section 8).

Earlier proposals that needed in-app language processing (typo distance, synonym detection, sentence-quality heuristics) have been replaced with fields in the import pack.

The data these recommendations are based on was checked against the two existing pages (the first page: 5 words; the second page: 70 words). Facts about the data that drive the design:

- Usefulness `ux` clusters in the middle: Example Book 2 has 1×1, 3×2, 1×3, 8×4, 19×5, 24×6, 12×7, 2×8. The usefulness lever has to have an effect inside the 4–7 band, not only at the extremes.
- The Czech field `cz` is almost always a list of several translations, sometimes with comments (`clatter` → "rachot; rachotit, třeskot; být bombardován"; `lull` → "…; zde: chvilkový klid: …"). 66 of 70 Example Book 2 entries contain `,` or `;`. **Czech cannot be an automatically checked answer.** It works only as a cue or as something you check yourself.
- Notes `n` average about 320 characters and reach about 1,000. Meanings `m` average about 150 characters and reach about 290. On a phone, the answer side has room for about one of these without scrolling.
- Examples `u` are real quotes from the source. The headword often appears in an inflected form ("it *clattered*", "*lulled* the crowd", "*mulled* over"). In 5 of 75 Example Book 2 examples the headword does not appear literally at all ("dwelt" for *dwelling*, "sifted" for *sifting*, "ledgers" for *ledger*). Any sentence extraction or cloze blanking needs the form list, and a check that the sense matches.
- Several entries are *sense-specific*: *lull* (here: a calm pause), *sift* (examine closely), *brisk* (energetic, quick), *dwindle* (shrink gradually). Sentences pulled from elsewhere in the PDF can easily use a different sense.
- Tags: `skip` (6, "Low priority" / "Skip — too niche" / "Learn 'shrink' instead"), `rep` (3, also in Example Book 1), `late` (2, not from this book).
- The existing palette already passes WCAG AA comfortably. On the card colour `#161E31`: text 14:1, muted `#8FA0BC` 6.3:1, accent 9.9:1, amber 9.4:1, red 6.6:1. Two styles fall short: faded "rare" forms (opacity .45) and the italic CZ hint (opacity .6).

---

## Summary of recommendations

1. **Two scheduled cards per word, each with a fixed format, and their own FSRS memory state:**
   - **R, "Understand" (receptive).** Default format is the new **sentence comprehension**: a real book sentence with the word highlighted and a **different sentence each review**. He reads it, optionally listens, reveals the Czech translation of the sentence plus the word's gloss, and rates himself 1–5.
     - Words with fewer than 3 usable sentences (films, articles) use a **word-only flip** instead: headword plus audio, then reveal.
     - The format is chosen per word when it enters training and is not changed later.
   - **P, "Produce" (productive).** A book sentence with the word blanked, plus a Czech cue → **type** the English word. The app compares the answer with the pack's accepted answers and pre-selects a suggested 1–5 rating; he confirms or changes it.
2. **P is unlocked, not created up front.** It starts only when the pack marks the word **active** (Claude's decision, typically for higher-usefulness words) **and** R has stability ≥ 7 days. Words marked recognise-only stay receptive-only.
   - Order of stages: (new) R cold, with feedback → R matures → P unlocks → (phase 2) R relaxes to a low retention target once P is mature.
   - **No multiple choice** as a scheduled exercise. Listening dictation, a confusables drill and forms practice are phase-2 session modes.
3. **1–5 scale (1 = best) mapped to FSRS:**
   - **1 → Easy, 2 → Good, 3 → Hard, 4 → Again, 5 → Again.**
   - Exception: 1 counts as Good until FSRS has fitted his personal parameters, so early intervals don't jump.
   - The pass/fail line sits between 3 and 4 and is drawn on the button row.
   - FSRS cannot tell 4 from 5, but the raw rating is stored and used outside FSRS. **5** gets the next in-session repeat sooner, opens the full learn view and counts as a full lapse. **4** asks an optional "confused with…?" question that feeds the leech/confusables logic.
4. **Level labels** (number plus one word plus one short line, all three always visible):
   - 1 · **Instantly**: sure, no effort.
   - 2 · **Knew it**: after a moment.
   - 3 · **Shaky**: got it, with effort or doubt.
   - 4 · **Vague**: half-knew it or wrong sense.
   - 5 · **No idea**: didn't know it.
5. **Rate the highlighted word, not the sentence.** The criterion shown on the card: "Would you have understood this word here, without help?" If he only worked it out from context, the honest rating is 3. Other unknown words in the sentence can be tapped and saved as candidates (phase 2). They never affect the target word's rating.
6. **Protection against muscle memory from "1 = fail" apps** (Anki's key 1 = Again):
   - never show a bare digit;
   - colour runs green (1) → red (5);
   - a visible gap separates "knew it" (1–3) from "didn't" (4–5);
   - fail buttons are outlined, pass buttons filled;
   - the label and an **Undo** stay visible for 4 s after every rating;
   - keyboard digits match the screen;
   - the first sessions show the legend.
7. **Audio (decided: neural TTS files):**
   - ▶ word and ▶ sentence on every card, preloaded for the next card. Recognition cards auto-play the word once on reveal.
   - **Never play anything on the question side of a P card** (it gives the answer away), or anything that speaks the target word before he has answered.
   - Speed 1× / 0.75×. Browser speech only as fallback.
   - Files are plain static assets, so they can be cached for offline use later.
8. **Usefulness sets the per-word desired retention** (ux 8–10 → 0.93 … 1–3 → 0.82) and orders new words and the backlog. Error rate is left to FSRS difficulty, so it isn't counted twice.
9. **Answer side uses progressive disclosure.**
   - Always visible above the pinned rating bar: Czech sentence translation (in R), word, respelling, ▶, Czech gloss, English meaning, forms.
   - Behind "More": the long note, other examples and tags. The note opens automatically after a 4 or 5.
   - The icon is hidden on the question side and shown on the answer side.
10. **Sessions:** 20-card blocks with "Continue +20". 5 new words per day by default, mixed in among reviews, paused automatically when there is a backlog. Never R and P of the same word on the same day. Every rating is saved immediately, and the last one can be undone.
11. **Leeches:** flagged after 4 lapses (ratings of 4–5 after the card has been learned). Not auto-suspended.
    - A leech screen shows the pack's **memory hint** and the pack's **confusables**. It offers demotion to receptive-only, lower usefulness, or suspend.
    - Leeches without a hint are listed in the **export for the next pack**, so Claude can write one.
12. **Thin app** (section 11): the app only normalises case, whitespace, punctuation and diacritics, then checks for an exact match against lists in the pack.
    - No fuzzy matching, lemmatisation, synonym detection or generated distractors.
    - The pack supplies, per word: active vs recognise-only, sentences with marked spans and Czech translations, accepted answers per blank, "near" answers (valid but not the target), confusables, memory hint and, later, multiple-choice distractors.
    - A typo simply doesn't match. He sees what he typed next to the answer, and can choose 3 instead of the suggested 4.
13. **Stats:** due today plus a 7-day forecast, real retention against target, per-book progress (known / producible / leech), per-word history (with the sentence shown and what he typed), leech list, a quiet 30-day activity strip. No XP, badges or loss-framed streaks.
14. **Mobile and accessibility:**
    - The five rating buttons fit one row at 375 px (about 60 × 56 px each), in the bottom third.
    - No swipes.
    - The typing field sits above the keyboard, with `autocorrect`, `autocapitalize` and `spellcheck` off.
    - Desktop keys: Space reveals, 1–5 rate, Enter accepts the suggested rating on P, R replays the word, S replays the sentence, M toggles More, U undoes.
    - Dark theme by default, light theme through `prefers-color-scheme`, no flip animation, `lang="cs"` on Czech text, results shown by colour plus label plus icon.

---

## 1. Exercise types

### What the evidence says, briefly

- **Retrieval beats re-reading.** In Karpicke & Roediger (2008), with foreign-vocabulary pairs, repeated retrieval drove long-term retention. Repeated study after the first successful recall added little. Learners could not tell which strategy was working. [K&R 2008]
- **Harder successful retrieval helps more.** Pyc & Rawson (2009) found that harder retrievals give more benefit than easy ones, as long as they still succeed. This is the "desirable difficulties" idea. [Pyc & Rawson 2009; Bjork]
- **Production practice builds more complete word knowledge** than recognition practice. Webb (2009) found productive learning of word pairs gave broader gains (spelling, productive meaning, grammar). Receptive learning gave larger receptive-meaning gains. If you can do only one, production is the better bet. Nakata's flashcard work found productive recall the most effective format overall, especially for spelling, while receptive recall learns more words per minute. [Webb 2009; Nakata 2013; Nakata & Webb 2016]
- **The four strengths of knowing a word form a hierarchy:** receptive recognition < receptive recall < productive recognition < productive recall (Laufer & Goldstein 2004). Passive recall, i.e. "see the English word, recall its meaning", was the best predictor of real reading and classroom performance. For a reader, the receptive card is the core and P is the upgrade.
- **Short-answer practice with feedback beats multiple choice** on later tests (Kang, McDermott & Roediger 2007). Multiple choice can plant false knowledge from distractors (Roediger & Marsh 2005). Feedback reduces this (Butler & Roediger 2008).
- **Errors followed by immediate correction are fine, even helpful.** Failed retrieval attempts followed by the answer improve later learning (Kornell, Hays & Bjork 2009). Errors made with high confidence are corrected especially well (Metcalfe 2017). This supports testing new words cold, and typed answers with immediate feedback.

### Exercise-by-exercise assessment

| Exercise | Trains | Evidence | Effort / fatigue | Phone fit | Verdict |
|---|---|---|---|---|---|
| **Sentence comprehension** (R card, default format; section 9) | Receptive recall *in context*, exactly the reading situation; varying sentences build a meaning that isn't tied to one sentence | Strong for receptive recall (testing effect; passive recall predicts reading). Risk: context lets him infer the meaning instead of recalling it, which needs a clear rating criterion | Low–medium, 8–15 s; reading a sentence costs a little more than a bare word | Excellent: read, tap, rate | **Core, phase 1** |
| **EN → meaning flip, word only** (R card, fallback format) | Receptive recall without context | Strong; also the hardest receptive test, because there is no context to lean on | Low, 5–10 s | Excellent | **Phase 1, only for words without ≥ 3 sentences** |
| **CZ → EN flashcard, self-graded** | Productive recall | Strong in principle, but self-grading production is lenient. The bare CZ cue is ambiguous ("rachotit, třeskat, bubnovat" could be *clatter*, *rattle*, *drum*) | Low | Excellent | **Not as a standalone card.** Czech is a cue inside the cloze |
| **Typed recall in a cloze sentence** (P card) | Productive recall plus spelling plus form in context | Strong (productive recall; short answer beats MC; the sentence fixes the sense) | Medium, 10–25 s; typing on a phone is the main cost | Good if the field avoids the keyboard and autocorrect is off | **Core for words the pack marks `active`, phase 1** |
| **Multiple choice** | Receptive recognition (the weakest level) | Weak for this level, and distractors can mislead | Very low | Excellent | **Not scheduled.** Possible phase-2 confusables drill, always with feedback. Distractors come from the pack (`confusables`, or a dedicated `distractors` list), never generated by the app |
| **Listening: hear → type** (dictation) | Spoken-form recognition plus spelling | Moderate. It targets a real gap: these words were met in print and never heard. Now practical thanks to good TTS files | Medium | Good | **Phase 2 session mode** |
| **Forms practice** | Morphology (datum → data, dwelt, mulled) | Low value for most entries | Medium | Fine | **Phase 2, irregular-flagged words only.** Phase 1: accept listed forms and show forms on the answer side |

**Why cloze rather than CZ → EN for production.** The sentence fixes the sense (*sift*, *brisk*, *lull*). It practises the word in the grammatical position where he will use it. It also follows SuperMemo's advice to use cloze and add context cues against interference [SuperMemo 20 rules]. The cue goes under the blank. Best cue: the **Czech translation of the whole sentence**, with the corresponding Czech word underlined; this comes free from the sentence pipeline in section 9. Fallback cue: the word's `cz` gloss.

**Rotate sentences in both R and P.** With PDF extraction most book words will have several sentences. Each review takes the least recently shown sentence, and R and P never show the same sentence within 7 days. This stops him memorising sentences instead of words, which the owner raised as a concern. Trade-off: Matuschak argues prompts should stay consistent so each review exercises the same retrieval. Here the target, *this word's meaning*, stays the same while the surface changes. That is the "varied practice" desirable difficulty, as long as sentence difficulty stays in a narrow band (see the selection rules in section 9). [Matuschak; Bjork]

---

## 2. Progression per word

**Recommendation:** use separate R and P cards, a fixed format per card, and unlock P when R is established. Do not change the exercise format inside one card.

- **Why separate cards.** FSRS keeps one memory state per card: difficulty D, stability S, retrievability R. These are estimated from that card's own rating history [ABC of FSRS]. Switching a card from a self-rated flip to a typed cloze changes how hard the task is. FSRS would read the drop in performance as forgetting. Anki's sibling cards (forward and reverse, scheduled independently, the sibling buried for the day) solve the same problem.
- **Where sentence comprehension fits.** It *is* the R card's presentation. It does not add a third card on top of R. Sentence comprehension and the word-only flip test the same knowledge (receptive meaning). The sentence version is closer to how he will meet the word, so it is the default. Varying the sentence is variation inside one format, which FSRS tolerates. Mixing word-only and sentence presentations on one card would not be.
  - **Format is chosen when the word enters training:** sentence if it has ≥ 3 usable sentences, otherwise word-only.
  - If sentences arrive later (e.g. the PDF is imported after the word), the word may switch *once*, logged as a format change. Optionally the R state can be reset to "review" with its current stability kept, so the extra difficulty shows up as a few weak ratings rather than a misread trend.
- **Progression stages:**

  | Stage | What he sees | Gate to next |
  |---|---|---|
  | New | R card tested **cold**, with a "New" badge. He met the word in the book and on the page already, and a failed first attempt followed by the answer still helps [Kornell, Hays & Bjork 2009] | First rating ≤ 3 (pass) after the in-session repeat |
  | Learning / Review | R card, sentences rotating | Pack marks the word `active`, and R stability ≥ 7 days → create P |
  | Producing | R and P scheduled independently, never on the same day | — |
  | Phase 2: Mastered | P stability ≥ 60 days → R moves to a low retention target (0.80): if he can produce it, he can recognise it | — |

- **Recognise-only vs active is Claude's call in the pack** (thin-app principle, section 11), not an app threshold. Recommended guidance for Claude when writing the pack:
  - `active` for words worth producing in his own English, typically `ux ≥ 5` and not `skip`;
  - recognise-only for words he only needs to understand when reading.

  On Example Book 2 the `ux ≥ 5` rule of thumb would make 57 of 70 words active. The word detail screen has a manual "also practise producing" switch that overrides the pack locally. Overrides are included in the export for the next pack.
- **`skip` words:** not introduced automatically. They can be included per word, and then they are recognise-only with ux ≤ 3 behaviour.
- **`rep` words:** one word record with several sources. Their sentences from both books join the same rotation pool.
- **Report errors per card type.** Fine at R but failing at P is a different situation from failing at R.

---

## 3. Grading UX

The owner's **1–5 scale (1 = best)** replaces Again/Hard/Good/Easy everywhere: R cards, P cards, every screen and every key. Section 10 has the full design: definitions, FSRS mapping, the 4 vs 5 split, muscle-memory safeguards, and what is being rated. This section covers the parts specific to each card type.

### Self-rated (R cards)

- After reveal, the **five rating buttons** fill the bottom bar. Nothing is preselected: he has to make the call himself.
- **The criterion is printed above the buttons**, in small text: "Rate the highlighted word: would you have understood it here, without help?" The Czech field lists several synonyms, so the bar is *meaning understood*, not *matched one translation*.
- The most common grading mistake under FSRS is calling a forgotten card a pass ("Hard" when it was really "Again"). It pushes intervals "unreasonably high" [Anki manual: Deck options; Anki forums]. On this scale, that mistake is rating **3** when it should be **4**. The 3|4 gap and the short definitions (section 10) exist to make that boundary unmissable.

### Typed answers (P cards): automatic suggestion on the same scale

Pipeline: normalise → **exact** comparison against lists in the pack → **suggest** a rating → he accepts (Enter, or "Next") or taps a different number. Per the thin-app principle, the app makes no language judgement of its own: no edit distance, no stemming, no synonym guessing.

1. **Normalise both sides (mechanical only):**
   - lowercase;
   - trim spaces and collapse runs of spaces;
   - remove punctuation, treating hyphen and apostrophe variants alike (*ever-green* = *ever green* = *evergreen*);
   - fold diacritics (the owner's allowed normalisation; it rarely matters for English answers).
2. **Lists from the pack, per sentence blank:**
   - `accept`: forms that are right *in this sentence*, e.g. *mulled over*, plus *mull over* if Claude judges the base form acceptable;
   - `near`: real English answers that fit the sentence but are not the target, e.g. *banging* for *clattering*;
   - word-level `forms`;
   - answers he accepted himself with an override (stored locally and exported back with the next pack).
3. **Suggested rating:**

   | Outcome | Suggested | Feedback |
   |---|---|---|
   | Matches `accept`, first try, no hint | **2 · Knew it** | "✓ Correct" |
   | Matches another entry in `forms` but not this blank's `accept` (*clatter* for *clattered*) | **2** | "✓ Right word. The sentence needs *clattered*" |
   | Hint used (icon → first letter → letter count) and then matched | **3 · Shaky** | — |
   | Matches `near` | One free retry, with the first letter shown. Match → **3**, otherwise → **4** | "Good word, but not the one we want" |
   | No match (including typos) | **4 · Vague** | What he typed, shown right above the correct answer in the same monospace layout, so he can spot a one-letter slip himself. The legend line reads: "Just a typo? Choose 3". The note opens |
   | "I don't know" button | **5 · No idea** | Correct answer; full learn view |

   Auto-grading never suggests **1**. "Instantly, no effort" is something only he can judge. He can upgrade a correct answer to 1 with a tap.
4. **Overriding** is just tapping another number. The suggested button shows a ring and the note "suggested". Overriding a fail to a pass (4/5 → 1–3) asks "Also accept *clatter* next time?". Yes adds it to his local `accept` list for that word and flags it for Claude to review in the next pack. Overrides are logged so that loose grading shows up in his stats.
5. Learning from errors depends on paying attention to the correction [Metcalfe 2017]. On 4/5 the answer view stays until he taps **Next**. It never advances by itself.

### Phone keyboard details that decide whether typing is bearable

`<input autocomplete="off" autocorrect="off" autocapitalize="none" spellcheck="false" enterkeyhint="go">`. iOS autocorrect otherwise "fixes" correct rare words (*ledger* → *leader*) and capitalises the first letter. QuickType suggestions can't be fully turned off from a page. This is a residual risk, not a blocker.

---

## 4. Card layout

Principles: show the minimum cue on the question side and give all retrieval cues as optional hints. On the answer side, put the minimum needed to *check* the answer above the fold and the material for *learning* behind one tap. This follows the minimum-information principle [SuperMemo rule 4]. Barcroft's TOPRA model also predicts that piling meaning-focused processing (long notes) onto every encounter takes resources away from the form–meaning link itself [Barcroft 2015].

### Where each field goes

| Field | R question (sentence / word-only) | P question (typed cloze) | Answer side |
|---|---|---|---|
| Headword `w` | Highlighted in the sentence / large on its own | Hidden (it's the answer) | Large |
| Book sentence | **The question**, word highlighted | **The question**, word blanked | Stays on screen, word highlighted |
| Czech sentence translation | Hidden | **Cue** under the blank, matching Czech word underlined | **First thing revealed** in R; repeated in P |
| ▶ sentence audio | Available (tap). Hearing the sentence doesn't give away the meaning | **Hidden** (it would say the answer) | Available |
| ▶ word audio | Available | **Hidden** | **Auto-plays once on reveal** |
| Respelling `s` | Hidden | Hidden | Under the headword, amber |
| Icon `ic` | Hidden (gives the meaning away) | Hint level 1 | Next to the headword |
| Czech gloss `cz` | Hidden | Fallback cue if there's no sentence translation | Highlighted line |
| Meaning `m` | Hidden | Hidden | Under the gloss |
| Forms | Hidden | Hidden | One line, rare forms faded (fix contrast to ≥ 4.5:1) |
| Source of sentence | Small caption ("Example Book 2, ch. 14") | Same | Same |
| Note `n` | Hidden | Hidden | **Collapsed behind "More"** with a one-line preview. **Opens automatically after a 4/5** |
| Other examples, tag text, usefulness | Hidden | Hidden | Inside "More" |
| Memory hint (pack field `hint`, for problem words) | Hidden | Hidden | Above the note when present; prominent on the leech screen |

The **rating bar is pinned to the bottom**. The answer content scrolls above it. "More" expands in place: no modal, no navigation.

---

## 5. Session design

### Length

- **Default block: 20 cards** (about 4–6 minutes): a phone session that fits a tram stop. The summary offers **"Continue +20"** and **"Done"**. No timer.
- "Due today" is a soft goal on the home screen, not a wall.

### New words

- **Default 5 new words per day** (adjustable 0–15). Anki's manual says 20 new cards a day leads to roughly 200 reviews a day at steady state, and warns that heavy early intake leads to burnout [Anki manual: Deck options]. Scaled down, 5 words (about 5 R cards plus about 3 P cards later) works out to roughly **40–70 reviews a day**, about 10–15 minutes.
- **Order new words by:**
  1. his chosen current source;
  2. then `ux` descending;
  3. then `seq`.
- **Auto-pause new words** when the backlog exceeds about 2 days of normal load. This follows the Anki manual's backlog advice.

### Mixing

- **Interleave** new cards among reviews (about 1 per 4). One large interleaved stack beats small massed stacks, even though 72% of learners believed the opposite [Kornell 2009].
- **Never R and P of the same word on the same day.** This is Anki's "bury siblings": seeing one answers the other. The same sentence is never shown twice within 7 days across R and P.

### Repeats within a session

- Rated **5**: comes back after about 3–4 other cards.
- Rated **4**: comes back after about 6–8 cards.
- At least once before the block ends, then the next day.
- Use a card-count gap, not Anki's 1 m / 10 m timers: phone blocks are short. FSRS-6 and later use same-day reviews in their model [ABC of FSRS].
- The repeat **uses a different sentence** when one is available.

### Usefulness and error rate in scheduling

- **Error rate:** FSRS already raises difficulty and shortens intervals for words that keep failing. A separate multiplier would count the same signal twice.
- **Usefulness:** set the **per-word desired retention** from `ux`:

  | ux | Desired retention | Effect |
  |---|---|---|
  | 8–10 | 0.93 | Clearly shorter intervals |
  | 6–7 | 0.90 | FSRS/Anki default |
  | 4–5 | 0.87 | Somewhat longer intervals |
  | 1–3 | 0.82 | Long intervals, cheap |

  The FSRS docs consider 0.70–0.97 reasonable, with workload rising steeply above about 0.90 [ABC of FSRS; Anki manual].
- **Backlog ordering:** (lowest retrievability relative to target) × usefulness weight.

### Interruptions and resume

- Every rating is **sent and saved immediately**:
  - one request per answer;
  - idempotent through a client-generated ID;
  - carrying the client timestamp;
  - including the **raw 1–5 value**, the mapped FSRS grade, the card format, the sentence ID, any typed text, and whether a hint was used.

  This shape also lets an offline queue be added later without redesign.
- "Session" is just a view over the review log since the block started. Reopening shows **"Resume — 12 of 20 left"**.
- **Undo last rating**: the toast after each rating, `U` on desktop, ↶ in the top bar.

### Leeches

- **Lapse** = a rating of 4 or 5 on a card that has already been learned. **Flag at 4 lapses**: Anki's default of 8 suits large decks [Anki manual: Leeches]. Also flag after two **5**s in a row on a card that had reached stability ≥ 21 days. That is total forgetting of something once known, usually a sign of interference.
- **Don't auto-suspend.** Show a **leech screen** in place of the answer side the next time the word comes up. The actions follow the Anki manual and SuperMemo: reformulate, separate confusables, or drop.
  1. **Memory hint**: the pack's `hint` for this word, shown large. If there isn't one yet, the word goes on the **"needs a hint" list** in the next export to Claude (thin-app principle). The screen says "Hint requested for the next pack".
  2. **Compare with confusables**: the pack's `confusables` (*shift*, *brisk*, *lull*) side by side, each with gloss and audio [SuperMemo rule on interference]. Words he picked in "confused with…?" are added to the export, so Claude can add them to `confusables`.
  3. **Make it easier**: demote P to recognise-only, or keep the first-letter hint on for P.
  4. **Lower usefulness**, or **suspend**.
- **ux ≤ 4 leeches default to "suspend"** with one tap.
- **Round trip:** leeches, his overrides and "hard sentence" flags all go into an export file. The next import pack from Claude answers them (new hint, revised sentences, extra `accept` entries). The app stores and displays; Claude judges.

---

## 6. Progress and motivation

Optimise for *knowing he's on track* and *seeing reading turn into vocabulary*. Evidence on streaks is mixed: they can build habits, but they shift motivation towards protecting the streak, and breaking a streak predicts quitting [The Decision Lab; Utrecht thesis 2025]. For a self-motivated single user, that loss aversion is all cost.

**Show (phase 1):**

| Stat | Why it's useful | Where |
|---|---|---|
| Due now / today, plus a 7-day forecast | Plan the day; see the cost of adding words | Home |
| Block result: distribution of 1–5 ratings | Honest, readable at a glance | Summary |
| **Real retention** over the last 30 days (share of mature reviews rated 1–3), R and P separately, against target | The only honest "is it working" number; also catches loose self-rating | Stats |
| **Per-source progress**: new / learning / known (stability ≥ 21 days) / leech, plus "can produce" | Ties practice to reading | Home (current source), Stats |
| **Per-word history**: date, card, sentence shown, rating 1–5, typed answer, hint, next due, recall probability | Diagnoses a word ("4 every time with sentence #3": maybe a bad sentence) | Word detail |
| Leech list | Action list | Stats |
| 30-day activity strip | Gentle consistency signal | Stats |

**Skip:** XP, levels, leagues, badges, confetti, streak counters, all-time vanity totals.

Derive a LingQ-style four-stage word status (new → recognised → familiar → known) from FSRS state, not set by hand [LingQ].

---

## 7. Mobile ergonomics and accessibility

### Thumb reach and controls

- Phones are held and operated in many ways, often with one thumb, and grips change constantly [Hoober 2013]. **All practice actions go in the bottom third.** Rarely used controls (end, undo, sound, settings) go at the top.
- **Five rating buttons in one row:** (375 − 2·16 margin − 4·6 gaps) / 5 ≈ **64 × 56 px** each. That clears WCAG 2.2's 24 px floor (2.5.8 AA) and the 44 px enhanced level (2.5.5 AAA). Add an extra gap of about 12 px between 3 and 4 as the pass/fail divider (section 10). On narrower phones (≤ 340 px) the labels shrink to the number plus a one-word label. They never drop to a bare number.
- **Reveal and Next sit in the same place**, full width, so the "tap to reveal" thumb position doesn't land on a rating by accident. After reveal there is a **250 ms input guard** on the rating row, so a double-tap on Reveal can't fire a rating.
- **No swipe gestures in phase 1.** Swipes are invisible, clash with Safari's edge-swipe navigation, and can't express five outcomes.
- **Typing screen:** put the sentence and input in the upper half, and use `visualViewport` to keep Check above the keyboard.

### Desktop keyboard

| Key | Action |
|---|---|
| Space | Reveal (R) |
| Enter | Check (P, in the input) · accept the suggested rating (P answer) · Next |
| 1 – 5 | Rate (the same number as on screen; 1 = best) |
| R / S | Replay word / sentence audio |
| M | Toggle "More" |
| H | Next hint (P) |
| U | Undo last rating |
| E | Word detail |
| Esc | End block → summary |

Space deliberately does **not** rate. In Anki, Space means "Good", and here it would be ambiguous. Digits are disabled while the text input has focus. "?" shows the shortcut overlay.

### Visual, motion, audio, screen readers

- **Dark theme by default** (the existing palette, verified AA). Light theme through `prefers-color-scheme`, with a manual override. Fix the two low-contrast styles.
- **The rating colour scale must work for colour-blind users and in both themes.** Use a green → teal → amber → orange → red ramp with different lightness steps, and keep the number plus label on each button, so colour is never the only cue.
- **Motion:** no 3-D flip. Swap content instantly with at most a 100–150 ms fade, and none under `prefers-reduced-motion`.
- **Text:** size in `rem`. The sentence is at least 19 px, with line-height 1.5.
- **Audio:** see section 8.
- **Screen readers:** `lang="cs"` on Czech text, `lang="en"` on English, an `aria-live` region for results, and real button labels ("Rate 2, Knew it").
- Keep the screen awake during a block where the Wake Lock API is available (nice-to-have).

---

## 8. Audio (decided: server-side neural TTS)

The owner's decision: neural TTS generated at import time and stored as audio files, one per word and one per sentence. Browser `speechSynthesis` is only a fallback. This removes the biggest weaknesses of the current pages: voice quality that depends on the device, iOS losing voices, and the gesture rules for `speechSynthesis` [WebKit/Apple reports]. Design implications:

- **Two play controls**, always in the same places:
  - ▶ next to the headword (word);
  - ▶ at the start of the sentence block (sentence).

  They are distinguishable by position and label ("word" / "sentence" for screen readers). Tapping again during playback restarts.
- **When audio plays by itself:**
  - **R card, on reveal:** the word, once. The reveal tap counts as a user gesture, which iOS requires for sound.
  - **R question side:** nothing by default, because the task is reading comprehension. A setting, "Read sentence aloud on question", turns R into a listening-comprehension variant for walks. In that setting the sentence text can also be hidden behind a "show text" tap (phase 2).
  - **P card question side: never, and no sentence ▶ either.** The sentence audio would say the answer. Both play controls appear on the answer side.
- **Preload** the next card's word and sentence files while the current card is showing, so playback starts instantly on mobile data. Files are small, and preloading 2 cards ahead is enough.
- **Format:** **MP3 or AAC (.m4a)** plays everywhere, including iOS Safari. Use Opus/WebM only if it has been tested on his devices.
- **Loudness:** normalise loudness across files at generation time, so words and sentences don't jump in volume.
- **Speed:** 1× and 0.75× using `playbackRate` with `preservesPitch`, in Settings and as a long-press on ▶.
- **Mute:** a sound toggle in the top bar, remembered per device. When muted, nothing auto-plays, and the ▶ buttons still work when tapped.
- **Offline later:** static, content-hashed URLs, so a service worker can cache them without any redesign.
- **Fallback:** if a file is missing, use `speechSynthesis`, triggered only from a tap. Keep the existing voice picker in Settings, for the fallback only.
- **Accent:** one consistent voice. See open questions.
- **Phase 2:** listening dictation (hear → type) becomes cheap to add on top of these files.

---

## 9. Sentence comprehension exercise

The owner's specification: show a real book sentence containing the word (highlighted); optionally play its audio; reveal the Czech translation of the whole sentence; rate yourself. Each review shows a **different** sentence, extracted from the book PDF.

### Where it sits

This is the **R card's default presentation** (sections 1 and 2). It is not a separate scheduled card. It trains receptive recall in context, the exact skill reading needs. P (typed cloze) still unlocks on top of it for useful words. The word-only flip remains only for words without enough sentences.

### What Claude must do when building the pack (the sentence choice decides whether the screen works)

Under the thin-app principle, every sentence decision is made by Claude in the import pack. The app only rotates sentences and filters them by location. Guidance for Claude:

1. **Same sense only.** Every chosen sentence must use the word in the sense the entry teaches. *Sift* as sieving flour, *clatter* as a word for clutter, *brisk* as cold weather are excluded. Wrong-sense sentences would teach confusion and make ratings meaningless.
2. **Mark the span** of the target in each sentence (any form: *mulled*, *dwelt*, *clattered*), so the app can highlight and blank it without matching text itself. Supply the per-blank `accept` and `near` lists (section 3).
3. **Length 8–35 words** (about ≤ 220 characters, at most about 4 lines on a phone). Trim to the clause containing the word, with "…". Skip sentences that are mostly dialogue tags or names.
4. **Context that isn't a giveaway.** Avoid sentences that define the word inline ("…clattering, making a loud noise…"). Aim for similar difficulty across a word's sentences.
5. **Location** (chapter/page) for every sentence. Then the app can do the one mechanical thing it owns here: **hide sentences beyond his reading position** (no spoilers; see open questions).
6. **Czech translation of the whole sentence**, with the **span of the Czech equivalent marked**, so the reveal can underline it ("…jsem *přemítal* o přiloženém plánu"). Translation quality matters more than usual here, because a wrong Czech sentence makes him doubt a correct understanding. Natural, not word-for-word.
7. **3–6 sentences per word** where the book allows. The original `u` sentence (where he met the word) is included.

App-side rules (mechanical):

- **Rotation:** least recently shown first. Never the same sentence twice within 7 days across R and P.
- **Format:** sentence format if there are ≥ 3 visible sentences, otherwise the word-only flip (section 2).
- **"Hide this sentence":** a local action that is also exported, so Claude can replace the sentence.

### Flow

1. **Question:** the sentence, word highlighted (bold plus accent underline, not colour alone). Optional ▶ sentence. A short source caption. Nothing else.
2. **Tap "Show translation"** (full-width, bottom).
3. **Revealed:**
   - the Czech sentence translation, with the equivalent underlined;
   - below it, a compact word strip: headword, respelling, ▶ word (auto-plays once), Czech gloss, and a one-line English meaning;
   - "More" for the note and forms.
4. **Rate 1–5** about **the highlighted word** (section 10d). On a 4 or 5, the note opens and a "confused with…?" chip row may appear (4 only, optional).
5. Next card.

### Risks and mitigations

- **Inferring from context inflates ratings.** Mitigation: the printed criterion ("…without help?") and the definition "3 · Shaky: got it from context or with effort". Real retention (section 6) shows if ratings run too high. A phase-2 option: every N-th review of a mature R card uses the word-only flip as a hard check, logged as a separate presentation and not used to update FSRS.
- **Sentences vary in difficulty.** Selection rules 3–4 narrow the range. If one sentence keeps getting worse ratings than the word's others, flag it in word detail ("sentence #3 often rated 4+") with "hide this sentence".
- **Rating the sentence instead of the word:** see section 10d.

---

## 10. Self-rating scale 1–5 (1 = best)

This is the owner's decision, the same direction as Czech school grades. It is used for every card and every screen.

### (a) Definitions for a small screen

Each button carries **number + one-word label**. The one-line definition shows in the legend (always visible during the first sessions, then on long-press or `?`). The words are chosen so that each level contains a different *kind* of experience, not just more or less of the same thing:

| | Label | One-line definition (legend) | FSRS |
|---|---|---|---|
| **1** | **Instantly** | Knew it at once, sure, no effort | Easy |
| **2** | **Knew it** | Knew it after a moment's thought | Good |
| **3** | **Shaky** | Got it, but with effort, doubt or help from context | Hard |
| ┆ | | *pass ┆ fail* | |
| **4** | **Vague** | Half-knew it, guessed wrong, or wrong sense | Again |
| **5** | **No idea** | Didn't know it at all | Again |

The Czech labels, if the UI is in Czech:

- 1 Hned (Hned, jistě, bez námahy)
- 2 Věděl (Věděl, po chvilce)
- 3 Vratké (S námahou / z kontextu)
- 4 Mlhavě (Napůl / špatný význam)
- 5 Vůbec (Vůbec jsem nevěděl)

The UI language is an open question.

### (b) Mapping to FSRS and using 4 vs 5

- **1→Easy, 2→Good, 3→Hard, 4→Again, 5→Again.** FSRS has exactly one failing grade and three passing ones [Anki manual: Studying], so the 3|4 line *is* the pass/fail line. It must match the honest question "would I have understood it while reading?"
- **About 1 = Easy.** The owner's definition of 1 ("no problem, totally sure") is what Anki calls Easy, and it will probably be his most common rating. Anki's guidance that Good should be the most common button assumes default parameters. FSRS fits its parameters to the user's own history, so consistent use matters more than which label is most frequent [ABC of FSRS].
  - Before there is enough history to fit his own parameters, heavy use of 1 will make intervals jump a lot.
  - Mitigation: until the first parameter fit, **map 1 → Good**, the same as 2. Store the raw 1, so nothing is lost when the fit happens. Then switch to 1 → Easy. Show the switch in Settings ("Using your personal parameters since …").
- **Keep 4 and 5 separate.** FSRS sees both as Again, but the raw value is stored and used elsewhere:
  - **In-session repeat:** 5 comes back sooner (after about 3–4 cards) than 4 (after about 6–8).
  - **Feedback:** 5 opens the full learn view (note, all forms, word audio again). 4 opens the note and offers the optional **"confused with…?"** chips: the pack's `confusables` for the word, plus "other…" which searches his own word list. This is the cheapest way to collect interference data for the leech screen and for the next pack.
  - **Leeches:** both count as a lapse. Two 5s in a row on a once-mature card is a separate early leech trigger (section 5).
  - **Stats and research:** the raw distribution shows whether 4 (partial knowledge) or 5 (nothing) dominates for a word. That tells "needs a hook" apart from "needs re-learning".
  - **A future scheduler** that can use partial-credit failures gets this data for free.
- **P cards** use the same scale with a suggested value (section 3). The mapping is the same.

### (c) Muscle memory: an inverted scale vs other apps

Two collisions:

1. **Number keys:** in Anki, key 1 = Again (fail).
2. **Spatial habit:** in most apps fail is on the left and in red.

Here the left-most button, 1, is the *best* rating. Recommendations:

- **Keep 1→5 left to right.** It matches the scale as the owner thinks of it (school grades) and the number keys. Reversing the visual order (5 … 1) would make the screen fight the keyboard and the labels.
- **Never a bare number.** Every button shows number plus label. The colour ramp runs **green (1) → teal → amber → orange → red (5)**, so the left side *looks* positive.
- **Make pass vs fail obvious by shape, not only colour:** buttons 1–3 are **filled**, 4–5 are **outlined**, with a wider gap and a thin "┆" divider between 3 and 4.
- **Confirmation after every rating:** a 4-second toast, "Rated 2 · Knew it — Undo", in that rating's colour. A wrong reflex tap costs one tap to fix.
- **The first 3 sessions** show the legend permanently above the buttons, as onboarding. After that it is collapsed, with `?` to bring it back.
- **Desktop:** digits match the screen. Space does *not* rate (section 7), so the Anki reflex "Space = Good" does nothing rather than doing the wrong thing.
- **Slip detection:** if a 1 is given less than 600 ms after reveal on a card whose last rating was 4–5, the toast stays longer and pulses once. This is a cheap guard against reflex taps without adding a dialog.

### (d) Rate the word or the sentence?

**The highlighted word only.**

- **Why:** the scheduling unit is the word card. If unknown *other* words or tricky syntax lower the rating, FSRS would see sentence difficulty as forgetting of the target word. That noise would shorten intervals for no reason and could create false leeches.
- **The criterion printed on the card:** "Rate the highlighted word: would you have understood it here, without help?"
- **"Understood the sentence, but only by guessing the word from context"** = **3 Shaky**, a pass. That is honest, and FSRS treats Hard as a pass with a shorter interval.
- **Other unknown words** in the sentence: in phase 2, tapping any word on the revealed side saves it as a **candidate** for his vocabulary (with the sentence as context). This turns the friction into input for the vocabulary list, without touching the target's rating.
- **If the Czech translation shows he understood the word but misread the sentence**, the word still gets its own rating. Optionally he can flag the sentence as "too hard" (it is then taken out of rotation).

---

## 11. Thin app: what the import pack must supply

The owner's principle: **the app is thin.** Every language judgement comes from Claude in the import pack. The app schedules, displays, records and does **mechanical** matching only:

- normalise case, whitespace, punctuation and diacritics;
- then check for an exact match against lists in the pack.

No fuzzy matching, lemmatisation, synonym detection, sentence scoring or generated distractors.

Consequences for the UI:

- **Typos aren't detected.** They are a no-match with a suggested 4. The UI makes the slip easy to *see* (typed answer shown directly above the correct one) and easy to *fix* (tap 3).
- **Every "the app knows that…" moment is a pack field.** If a field is missing, the feature hides itself instead of guessing. No `near` list means no "good word, not the one we want" retry. No `hint` means the leech screen asks for one in the next pack.
- **A round trip closes the loop.** An **export** (button in Settings, or an API the import tooling calls) lists:
  - leeches without a hint;
  - overrides he accepted;
  - "confused with…" picks;
  - hidden sentences;
  - words where his manual active/recognise toggle disagrees with the pack.

  Claude's next pack answers them.

### Pack fields the training UI relies on

Existing fields (`w`, `forms`, `s`, `say`, `m`, `cz`, `u`, `n`, `ic`, `ux`, `tag`) stay. New or newly required:

| Field | Level | Used by | Notes |
|---|---|---|---|
| `mode`: `active` \| `recognise` | word | P unlock (section 2) | Claude's call; `ux ≥ 5` and not `skip` is a reasonable default rule |
| `sentences[]` | word | R sentence format, P cloze | Each sentence: `text`, `span` (char offsets of the target), `cz` (Czech sentence), `cz_span` (Czech equivalent), `loc` (chapter/page), `accept[]`, `near[]` |
| `confusables[]` | word | "Confused with…?" chips, leech compare, phase-2 MC | Each with headword and short gloss, e.g. *dwindle: ubývat*, *brisk: svižný* |
| `hint` | word, optional | Leech screen, answer side | Memory hint for problem words. Can be requested through the export |
| `distractors[]` | word, phase 2 | Multiple-choice confusables drill | Only if MC is ever added; plausible wrong meanings or words |
| `audio` | word and sentence | Section 8 | File references produced by the TTS step at import |
| `irregular` | form, phase 2 | Forms drill | Marks forms worth practising (*strata*, *slid*) |

---

## Proposed phase-1 screens

1. **Today (home):** due count, Start, current source progress, 7-day forecast, Resume.
2. **Practice: R question (sentence comprehension)**, and its word-only variant.
3. **Practice: R revealed:** Czech sentence, word strip, 1–5 rating.
4. **Practice: P question (typed cloze)**, and **P answer** with what was typed and the suggested rating.
5. **Leech intervention** (in place of the answer side).
6. **Block summary.**
7. **Word detail:** history, sentences pool, usefulness, accepted answers, hook, toggles.
8. **Stats.**
9. **Settings:** new/day, block size, audio (speed, auto-play, fallback voice), theme, current source and reading position, legend, **export for the next pack** (leeches without a hint, overrides, confusions, hidden sentences).

### Wireframes (phone, about 375 px wide)

**2. R question: sentence comprehension**

```
┌─────────────────────────────────────┐
│ ✕          7 / 20         🔊   ↶    │  ← end · progress · sound · undo
│ ▓▓▓▓▓▓▓░░░░░░░░░░░░░                │
│ UNDERSTAND                  ● new   │  ← mode label; badge if new
│                                     │
│  ( ▶ sentence )                     │  ← optional; never auto on R
│                                     │
│  "She ══mulled over══ the old plan  │  ← word bold + underline
│   all evening, looking for a        │     (≥19px, line-height 1.5)
│   fix."                             │
│                                     │
│   — Example Book 2, ch. 14          │  ← source caption, muted
│                                     │
│                                     │
│                                     │
│  Rate the highlighted word:         │  ← criterion, small muted
│  would you understand it unaided?   │
│ ┌─────────────────────────────────┐ │
│ │        Show translation         │ │  ← ≥56px, bottom
│ └─────────────────────────────────┘ │
└─────────────────────────────────────┘
```

**3. R revealed: Czech sentence and rating (1 = best)**

```
┌─────────────────────────────────────┐
│ ✕          7 / 20         🔊   ↶    │
│ ▓▓▓▓▓▓▓░░░░░░░░░░░░░                │
│  "She ══mulled over══ the old plan  │  ← sentence stays (smaller)
│   all evening, …"       ( ▶ )       │
│ ┌─────────────────────────────────┐ │
│ │ CZ  Celý večer ══přemítala      │ │  ← sentence translation
│ │     nad══ starým plánem a       │ │     equivalent underlined
│ │     hledala řešení.             │ │     (lang="cs")
│ └─────────────────────────────────┘ │
│ [📖] mull over  MUL OH-ver  ( ▶ )   │  ← word strip; ▶ auto-plays once
│      dlouze přemýšlet, rozvažovat   │  ← cz gloss
│      To study something closely…    │  ← 1-line meaning
│ ▸ More: note · forms                │  ← auto-opens after 4/5
│                                     │
│ ┌─────┬─────┬─────┐ ┆ ┌─────┬─────┐ │
│ │  1  │  2  │  3  │ ┆ │  4  │  5  │ │  ← 1–3 filled, 4–5 outlined
│ │Inst-│Knew │Shaky│ ┆ │Vague│ No  │ │     green → red ramp
│ │antly│ it  │     │ ┆ │     │ idea│ │     ~60×56px each
│ └─────┴─────┴─────┘ ┆ └─────┴─────┘ │
└─────────────────────────────────────┘
  after tap → toast: "Rated 2 · Knew it    Undo"  (4 s)
```

**2b. R question: word-only variant (words with < 3 sentences)**

```
│ UNDERSTAND                          │
│                                     │
│              ledger                 │  ← ≥28px
│               ( ▶ )                 │
│                                     │
│ ┌─────────────────────────────────┐ │
│ │           Show answer           │ │
│ └─────────────────────────────────┘ │
  revealed: word strip + CZ gloss + meaning + 1 example, then the 1–5 row
```

**4. P question: typed cloze**

```
┌─────────────────────────────────────┐
│ ✕          8 / 20         🔊   ↶    │
│ ▓▓▓▓▓▓▓▓░░░░░░░░░░░░                │
│ PRODUCE · Example Book 2            │  ← no audio playback here
│                                     │
│  "The rain was _________ down       │
│   on the tent all night."           │
│                                     │
│  CZ  Celou noc na stan ══dunel══    │  ← Czech sentence as cue
│      déšť.                          │
│ ┌─────────────────────────────────┐ │
│ │ clattering|                     │ │  ← input, upper half
│ └─────────────────────────────────┘ │
│  [ hint ]             [ I don't know ]│
│ ┌─────────────────────────────────┐ │
│ │             Check               │ │  ← stays above keyboard
│ ├─────────────────────────────────┤ │
│ │  q w e r t y u i o p   (kbd)    │ │
└─────────────────────────────────────┘
```

**4b. P answer: suggested rating**

```
┌─────────────────────────────────────┐
│ ✓ Correct                           │  ← colour + icon + word
│  "The rain was ══clattering══ down  │
│   on the tent all night."  ( ▶ )    │  ← audio now allowed
│ [🌧] clattering  KLAT-ring  ( ▶ )    │
│      clattering · clattered         │
│      rachotit; o dešti: bubnovat    │
│ ▸ More                              │
│                                     │
│ ┌─────┬─────┬─────┐ ┆ ┌─────┬─────┐ │
│ │  1  │ (2) │  3  │ ┆ │  4  │  5  │ │  ← (2) ringed = suggested
│ └─────┴─────┴─────┘ ┆ └─────┴─────┘ │
│ ┌─────────────────────────────────┐ │
│ │        Next  (accept 2)         │ │  ← Enter; or tap another number
│ └─────────────────────────────────┘ │
└─────────────────────────────────────┘

No-match variant (top):
│ ✗ Not quite                          │
│   you typed:  clatring               │  ← monospace, aligned
│   answer:     clattering             │  ← suggested (4); note auto-opens
│   Just a typo? Choose 3.             │  ← no fuzzy matching in app
```

**6. Block summary**

```
┌─────────────────────────────────────┐
│                 Done                │
│   20 cards · 5 min · 2 new words    │
│                                     │
│   1 ▇▇▇▇▇▇ 6                        │  ← rating distribution,
│   2 ▇▇▇▇▇▇▇ 7                       │     same colours as buttons
│   3 ▇▇▇ 3                           │
│   ┆                                 │
│   4 ▇▇ 2                            │
│   5 ▇▇ 2                            │
│                                     │
│   Look at these again               │
│   ┌───────────────────────────────┐ │
│   │ 5 mull over  P  typed "mull"  │ │  ← tap → word detail
│   │ 5 dwindle    R  ⚠ leech       │ │
│   │ 4 sift       R  ≈ "shift"     │ │  ← from "confused with…?"
│   │ 4 clattering P                │ │
│   └───────────────────────────────┘ │
│   Due now 23 · tomorrow 31          │
│   Example Book 2: 52 / 70 known     │
│ ┌───────────────┐ ┌───────────────┐ │
│ │     Done      │ │ Continue +20  │ │
│ └───────────────┘ └───────────────┘ │
└─────────────────────────────────────┘
```

**7. Word detail**

```
┌─────────────────────────────────────┐
│ ←  Word                         ✎   │
│ [🌙]  lull                ( ▶ )     │
│       LUL · lull · lulled           │
│ CZ  pauza, klid; ukolébat…          │
│ A short calm period between two…    │
│ Usefulness  ▮▮▮▯▯▯▯▯▯▯ 3  [ – ][ + ]│  ← sets retention target
│ Sources     Example Book 2          │
│ ── Understand (R) · sentences ────  │
│ Recall chance 91% · next in 9 d     │
│ 2 1 4 2 1   (tap for details)       │  ← raw ratings, oldest→newest
│ ── Produce (P) ───────────────────  │
│ Not unlocked (usefulness < 5)       │
│ [ ] Also practise producing         │
│ ── Sentences (6) ─────────────────  │
│ #1 "…a lull in the long storm…"     │
│    shown 3× · avg 1.7     [ hide ]  │
│ #4 "…the lull lasted an hour…"      │
│    shown 2× · avg 3.5 ⚠   [ hide ]  │  ← hard-sentence flag
│ Hint: — (requested for next pack)   │  ← pack field; never app-made
│ Accepted (pack + mine): lull, …     │
│ ▸ Note                              │
│ [ Suspend ]           [ Reset ]     │
└─────────────────────────────────────┘
```

---

## Open questions for the product owner

1. **Spoilers:** may sentences come from the whole PDF, or only from pages he has already read (needs a "reading position" per book)? Phase-1 default suggestion: only finished books, plus the current book up to a position he sets.
2. **Sense filtering (Claude, in the pack):** confirm that Claude drops sentences where the word has a different sense, with "hide sentence" in the app as the manual safety net. And which sense is taught for book-specific senses (*lull* = a pause in activity): the book sense, the everyday sense (soothe), or both as separate entries?
3. **Sentence translation:** confirm Claude supplies the Czech sentence translations with the marked Czech equivalent (`cz`, `cz_span`). Should he be able to edit them in the app, or only report them through the export?
4. **Mapping 1 → Easy:** OK to treat 1 as Good until FSRS has fitted his personal parameters, then as Easy? Or should 1 and 2 both always be Good (more conservative intervals)?
5. **Legend language:** Czech or English labels on the rating buttons ("Hned / Věděl / Vratké / Mlhavě / Vůbec" vs "Instantly / Knew it / Shaky / Vague / No idea")?
6. **"Confused with…?" after a 4:** wanted, or one tap too many? (Proposed: optional chips that can be ignored.)
7. **Active vs recognise-only:** this is now Claude's `mode` field. What guidance should Claude follow? `ux ≥ 5` and not `skip` makes 57 of 70 Example Book 2 words active; `ux ≥ 6` gives 38.
8. **Typing on the phone:** acceptable for P cards, or a "think, reveal, self-rate" mode on the phone and typing only on desktop? (Self-rating production is known to be lenient.)
9. **Minimum sentences for the sentence format:** 3 is proposed. Words from films and articles will mostly fall back to word-only cards. Are there other sources (subtitles, article text) to extract sentences from?
10. **Listening variant:** wanted soon? (R with the sentence auto-played and the text hidden, for walks.)
11. **Daily budget:** a realistic minutes-per-day target sets the new-words default (5 is proposed).
12. **`skip` words:** never trained, or receptive-only at a low retention target?
13. **TTS voice accent:** US or British? The respellings read as US-leaning.
14. **Usefulness over time:** will he edit `ux`? If so, the word detail control belongs in phase 1.
15. **Reminders:** none in phase 1 (push needs HTTPS plus a service worker on the self-hosted origin). Is that OK?
16. **HTTPS on the LAN:** later features (service-worker offline cache, push) need a secure context. Is HTTPS on the home network planned? (Not needed for phase 1.)
17. **Typos without fuzzy matching:** OK that a typo is a suggested 4 that he downgrades to 3 himself? Alternatively, Claude could put known likely misspellings into `near` or `accept`, but that seems a poor use of the pack.
18. **Export round trip:** a manual "export for Claude" file, or should the import tooling pull it automatically through the API?

---

## Sources

Research on learning and memory
- Karpicke, J. D. & Roediger, H. L. (2008). The critical importance of retrieval for learning. *Science* 319. https://pubmed.ncbi.nlm.nih.gov/18276894/ · summary: https://source.washu.edu/2008/02/practicing-information-retrieval-is-key-to-memory-retention-study-finds/
- Pyc, M. A. & Rawson, K. A. (2009). Testing the retrieval effort hypothesis. *J. Memory and Language* 60. https://notes.andymatuschak.org/z5zckzyAM1DwEQqt94qMpsd · follow-up: https://link.springer.com/article/10.3758/s13423-013-0434-z
- Bjork, R. A. — Desirable difficulties (overview). https://www.psychologicalscience.org/observer/desirable-difficulties · https://en.wikipedia.org/wiki/Desirable_difficulty
- Kornell, N. (2009). Optimising learning using flashcards: spacing is more effective than cramming. *Applied Cognitive Psychology* 23. https://cir.nii.ac.jp/crid/1361699994055985152
- Kornell, N., Hays, M. J. & Bjork, R. A. (2009). Unsuccessful retrieval attempts enhance subsequent learning. *JEP: LMC* 35(4). https://pubmed.ncbi.nlm.nih.gov/19586265/
- Kang, S., McDermott, K. & Roediger, H. L. (2007). Test format and corrective feedback modify the effect of testing on long-term retention. https://www.gwern.net/docs/spaced-repetition/2007-kang.pdf
- Roediger, H. L. & Marsh, E. J. (2005). The positive and negative consequences of multiple-choice testing. https://scholars.duke.edu/publication/677292
- Butler, A. C. & Roediger, H. L. (2008). Feedback enhances the positive effects and reduces the negative effects of multiple-choice testing. https://pubmed.ncbi.nlm.nih.gov/18491500/
- Metcalfe, J. (2017). Learning from errors. *Annual Review of Psychology* 68. https://bibbase.org/network/publication/metcalfe-learningfromerrors-2017 · https://www.learningscientists.org/blog/2019/10/17-1

Second-language vocabulary research
- Nakata, T. (2013). Optimising second language vocabulary learning from flashcards (PhD thesis, VUW). https://linguistlist.org/issues/24/3797 · https://ir.wgtn.ac.nz/handle/123456789/29297
- Nakata & Webb (2016), cited via: https://pmc.ncbi.nlm.nih.gov/articles/PMC9485613 and https://frontiersin.org/articles/10.3389/fpsyg.2022.984211/pdf
- Webb, S. (2009). The effects of receptive and productive learning of word pairs on vocabulary knowledge. *RELC Journal* 40(3). https://ir.wgtn.ac.nz/handle/123456789/23701
- Mondria, J.-A. & Wiersma, B. (2004). Receptive, productive, and receptive + productive L2 vocabulary learning. In Bogaards & Laufer (eds.), *Vocabulary in a Second Language*. https://benjamins.com/catalog/lllt.10
- Laufer, B. & Goldstein, Z. (2004). Testing vocabulary knowledge: size, strength, and computer adaptiveness. *Language Learning* 54(3). https://lexisite.com/catss/catss-info · https://cris.haifa.ac.il/en/publications/size-and-strength-do-we-need-both-to-measure-vocabulary-knowledge/
- Barcroft, J. (2015). *Lexical Input Processing and Vocabulary Learning* (TOPRA model). https://benjamins.com/catalog/lllt.43

Spaced repetition tools and practice
- Anki Manual — Deck options (new cards vs reviews, display order, FSRS desired retention, Hard misuse, burying). https://docs.ankiweb.net/deck-options.html
- Anki Manual — Studying (answer buttons, shortcuts). https://docs.ankiweb.net/studying.html
- Anki Manual — Leeches. https://docs.ankiweb.net/leeches.html
- ABC of FSRS (open-spaced-repetition wiki). https://github.com/open-spaced-repetition/awesome-fsrs/wiki/ABC-of-FSRS
- Anki forums — Pass/Fail grading as default (Expertium's analyses, incl. #120 "inconclusive"). https://forums.ankiweb.net/t/pass-fail-grading-as-default/34147/120
- Anki forums — How to prevent users from misusing Hard. https://forums.ankiweb.net/t/how-to-prevent-users-from-misusing-hard-ideas-are-welcome/49092?page=8
- FSRS Helper / Easy Days and load balancing. https://github.com/open-spaced-repetition/fsrs4anki-helper
- Woźniak, P. — Effective learning: Twenty rules of formulating knowledge (SuperMemo). https://www.supermemo.com/en/archives1990-2015/articles/20rules
- Matuschak, A. — How to write good prompts. https://andymatuschak.org/prompts/
- Settles, B. & Meeder, B. (2016). A trainable spaced repetition model for language learning (Duolingo HLR). https://preview.aclanthology.org/landing_page/P16-1174 · https://github.com/duolingo/halflife-regression
- WaniKani community — typo tolerance threads. https://community.wanikani.com/t/wrong-answers-due-to-typos/55872 · https://community.wanikani.com/t/a-little-leeway-on-wrong-answers/54818
- Clozemaster forum — Text or multiple choice? https://forum.clozemaster.com/t/text-or-multiple-choice/22764 · https://forum.clozemaster.com/t/feature-request-75-mastered-turn-on-text-input/25096
- Quizlet Learn (progression MC → written). https://thejournal.com/articles/2017/03/09/quizlet-debuts-study-feature-that-helps-students-study-efficiently.aspx · https://medium.com/tech-quizlet/how-quizlet-creates-trickier-questions-313317a6f47f
- LingQ — LingQ statuses and known words. https://lingq-support.groovehq.com/help/can-you-explain-a-lingqs-status · https://lingq-support.groovehq.com/help/understanding-lingq-statistics

Motivation and gamification
- The Decision Lab — Streak creep: when gamified engagement mechanics backfire. https://thedecisionlab.com/insights/consumer-insights/streak-creep-the-perils-of-too-much-gamification
- Utrecht University thesis (2025) on Duolingo gamification pressures. https://studenttheses.uu.nl/handle/20.500.12932/48993

Mobile ergonomics, accessibility, web platform
- Hoober, S. (2013). How do users really hold mobile devices? UXmatters. https://uxmatters.com/mt/archives/2013/02/how-do-users-really-hold-mobile-devices.php
- W3C — Understanding WCAG 2.2 SC 2.5.8 Target Size (Minimum). https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html
- WebKit bug / Apple forums — speech synthesis on iOS requires a user gesture; voices missing (motivation for server-side TTS). https://bugs.webkit.org/show_bug.cgi?id=216684 · https://developer.apple.com/forums/thread/764438 · https://readium.org/speech/docs/WebSpeech.html
