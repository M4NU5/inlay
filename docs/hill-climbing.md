# Hill climbing cleanup and latency

This page records three measured tuning passes on the Linux server (WSL2, CPU only): cleanup prompt quality, prompt length, and release-to-insert latency. Only the winning prompt is committed. The scoring rig is kept outside the repository, because [AGENTS.md](../AGENTS.md) does not allow behavioral harnesses in the tree.

## Setup

- Corpus: a copy of a real take archive (2,202 takes, 1,041 with `inference.wav`). That server ran with proofreading disabled, so every take has a raw Parakeet transcript and no previous Qwen output. The daily server, its archive and its settings were only read, never changed.
- Sample: 100 random dictation takes with audio, plus all 39 takes whose raw text contains a spoken-correction cue (`sorry`, `I mean`, `erm`, `er`, `correction`). The same 139 takes were used for every prompt.
- Pipeline: each raw transcript goes through the server's own `cleanTranscript`, dictionary and list formatting, then the real `inlay-text-engine` helper with the pinned Qwen3-4B-Instruct-2507 Q4_K_M GGUF, then the dictionary again and `evaluateCorrection` from `Server/src/domain/correction.ts`. That is the same path as `proofread()` in `generation-service.ts`, including the 18 s server deadline.
- Machine: Intel i7-13700K, 16 vCPUs given to WSL2, no GPU. Experiments ran one at a time at `nice 10`. Other agents' builds were running on the same machine for much of the session (load average 5 to 19, recorded per run), so absolute times are noisier than a quiet machine would give.

## Hill 1: cleanup prompt quality

**Benefit.** When Qwen's rewrite is rejected or fails, the user gets the uncleaned text, fillers included. When a rewrite passes but is wrong, the user gets a quiet error. Both happen on ordinary takes.

**Metrics.**

- Fallback rate: the share of takes where `correction.ts` rejects the rewrite or the helper fails. It is free and needs no labels.
- Golden set: 38 hand-checked takes from the cue subset. 24 are apologies or asides that must be kept ("Sorry, try that again"); 14 are real spoken corrections. Expectations were written from the raw text before any model output was read. A take passes when the delivered text matches an expectation after case and punctuation are ignored.
- Judge: Claude Sonnet 5.5 grades each delivered text as faithful or not, and whether an allowed edit was missed. "Clean" means faithful with nothing missed. Re-judging identical outputs moved the counts by up to about 5 points, so smaller differences are noise.

**Knobs.** Wording, section order and few-shot examples.

| Prompt                                                                            | Tokens | Fallback |                   Golden |  Judge clean |
| --------------------------------------------------------------------------------- | -----: | -------: | -----------------------: | -----------: |
| v0, previous default                                                              |    218 |    10.1% | 16/38 (0/14 corrections) | 54.7%, 54.7% |
| v1, "remove hesitation sounds, including at the start"                            |    235 |    10.1% |             18/38 (2/14) | 70.5%, 70.5% |
| v2, v1 plus a real-style "or sorry" example and apology examples                  |    273 |    12.9% |             18/38 (2/14) |        61.9% |
| v3, v1 with the Output section first                                              |    235 |    10.8% |             16/38 (1/14) |        64.7% |
| v4, v1 plus "never drop the cue and keep the old phrase", keep swearing and names |    310 |    14.4% |             18/38 (3/14) |        69.8% |

**What the numbers say.**

- The previous prompt never said to remove hesitation sounds outright. Qwen left a leading "Uh" or "Um" in about one take in ten, and the judge flagged 32% of takes as missing an edit. One sentence fixed most of that: missed edits fell to 14% and clean output rose from 55% to 70%.
- Spoken corrections stay weak at this model size: at most 3 of 14 were right. The common failure is deleting the cue ("sorry") while keeping the abandoned phrase, so "deploy it to staging, or sorry, to production" becomes "deploy it to staging or to production". That passes the validator, because only a cue word was removed. Extra rules and examples (v2, v4) did not fix it and raised the fallback rate.
- The validator rejects some correct rewrites. A standalone trailing "Um" or "Hmm." counts as a removed sentence, and a correctly resolved "X, or sorry, Y" can fail the 0.75 length-ratio check. Prompt wording cannot change that. It is a candidate change for `correction.ts`, not made here.

## Hill 2: the shortest prompt that holds quality

**Benefit.** The helper clears its KV cache and decodes the whole system prompt again for every take (`llama_memory_clear` in `TextEngine/worker.cpp`). Every token removed is prefill time saved on every dictation.

**Metric.** Prompt tokens (Qwen3 tokenizer), with hill 1's fallback, golden and judge scores held flat or better than v1.

**Knobs.** Removing redundant clauses, examples and section headers.

| Prompt                                       | Tokens | Fallback | Golden |  Judge clean | Mean proof time |
| -------------------------------------------- | -----: | -------: | -----: | -----------: | --------------: |
| v1                                           |    235 |    10.1% |  18/38 | 70.5%, 70.5% |          6.08 s |
| s1, trimmed v1 (four sections, two examples) |    182 |     7.2% |  19/38 | 74.8%, 68.3% |          5.25 s |
| s2, three sentences, one example             |    123 |    10.1% |  19/38 |        59.7% |          4.54 s |

s1 holds quality and is 23% shorter than v1 (17% shorter than the old default). s2 is faster but the judge marks a quarter of its output unfaithful, so it fails the hold condition. The committed default is s1 with "role markers" restored in the Output rule, because that phrase guards against a dictated "System:" line and the corpus has no such takes. That version (186 tokens) was rerun on the full sample: fallback 8.6%, golden 21/38, judge clean 69.1%, and 130 of 139 delivered texts identical to s1.

## Hill 3: release-to-insert latency

**Benefit.** The time from releasing the dictation key to text appearing is how responsive Inlay feels. On this machine, Qwen accounts for 74 to 94% of it.

**Metric.** Milliseconds per take, split into speech (Parakeet) and proof (Qwen plus validation). Quality is checked with the hill 1 scores so a faster setting cannot win by being worse.

**Knobs tried.** Helper thread counts, GGUF quantisation, prompt length, and stage overlap. Context size is a compile-time constant in `TextEngine/worker.cpp` (8,192 tokens) and was not rebuilt or measured.

Speech threads (40 takes with audio, Parakeet only; transcripts matched the archive in 40 of 40):

|             Threads |     Mean |    p50 |      p95 |
| ------------------: | -------: | -----: | -------: |
|                   4 | 1,677 ms | 940 ms | 4,588 ms |
| 8 (current default) | 1,011 ms | 569 ms | 2,757 ms |
|                  12 |   947 ms | 557 ms | 2,627 ms |

Proof threads (40 takes, Q4_K_M): 4 threads averaged 6.83 s, 8 threads 4.64 s, 12 threads 6.75 s and 16 threads 6.92 s. The 12- and 16-thread runs overlapped a load spike (load average about 19), and each timed out on 4 takes. Above 8 threads, Qwen competes with everything else on 16 vCPUs.

Quantisation and prompt, interleaved: every take ran through each configuration back to back in rotating order, so background load (load average 8 to 10) hit each one equally. These are 24 takes with no failures.

| Configuration           | Mean proof |    p50 |     p95 | Quality on the 139-take sample                                |
| ----------------------- | ---------: | -----: | ------: | ------------------------------------------------------------- |
| Q4_K_M, previous prompt |     5.24 s | 4.46 s |  9.21 s | fallback 10.1%, golden 16/38, clean 54.7%                     |
| Q4_K_M, new prompt      |     4.84 s | 4.26 s |  8.69 s | fallback 8.6%, golden 21/38, clean 69.1%                      |
| Q3_K_M, new prompt      |     6.32 s | 5.62 s |  9.97 s | fallback 7.9%, golden 21/38, clean 73.4%\*                    |
| Q5_K_M, new prompt      |     7.86 s | 7.13 s | 12.30 s | fallback 17.3% (14 timeouts), golden 22/38, unfaithful 9.4%\* |

\* Measured with the 182-token draft, which differs from the committed prompt only by "role markers".

**Trade-off.**

- Q4_K_M at 8 threads stays. Q3_K_M is smaller but slower on this CPU, with no quality gain. Q5_K_M was the most faithful when it finished (9.4% unfaithful against 13 to 15% for Q4), but it is 62% slower and missed the server's 18 s deadline on one take in ten under load. Missed deadlines fall back to uncleaned text, so for users Q5 is worse. It would also need a new pin in `Server/src/generated/model-pins.ts`.
- The shorter prompt is the only change that is both faster and better: about 8% less proof time in the paired run.
- The 123-token prompt (s2) was faster still (4.54 s mean against 5.25 s in the sequential runs), but it lost too much quality to ship.

End to end, 40 real takes through the helpers (speech, then cleaner, dictionary, lists, Qwen and validation), run one after the other:

| Prompt   | Speech mean | Proof mean | Total mean | Total p50 | Total p95 |
| -------- | ----------: | ---------: | ---------: | --------: | --------: |
| Previous |    1,325 ms |   6,478 ms |   7,803 ms |  6,646 ms | 19,403 ms |
| New      |    1,008 ms |   4,695 ms |   5,704 ms |  4,475 ms | 12,235 ms |

The speech column moved too, so part of that 27% is lower background load during the second run. The paired 8% figure above is the conservative one.

**Stage overlap.** `withStage` already lets speech for one take run while Qwen works on the one before. A single take cannot overlap, because proof needs the finished transcript. In a three-take burst through the dev server, overlapped speech ran about twice as slowly because both helpers want 8 threads. The burst finished in about 20 s against about 22 s for the same takes one at a time.

**Dev server check.** Eight archived takes were uploaded through the API to an isolated `bun run dev` instance on port 8392, using the committed default prompt. All eight completed. Release to final text took 3.2 to 10.7 s, within about 50 ms of the server's own `generation_processed` totals. Speech took 0.2 to 2.5 s and Qwen 3.0 to 8.4 s.

**Not tried, worth doing next.**

- Cache the system prompt's KV state between takes in `TextEngine/worker.cpp` instead of clearing it. The prompt is identical across takes, so its prefill could be paid once. This needs a helper rebuild.
- Let a trailing hesitation sentence ("Um", "Hmm.") through `correction.ts`, and make the length-ratio check allow verified corrections. Both cause false rejections today.
- Give Qwen fewer threads while speech for another take is running. That would reduce the contention seen in bursts.

## Rerunning

The rig is a handful of Bun scripts outside the repository. They import the domain modules from a checkout, spawn the release helpers, and write JSON lines.

1. Copy a take archive (`generations/`) somewhere private. Never point the rig at a live archive.
2. Build a corpus from the copy: raw text, then the server's cleaner, dictionary and list formatting.
3. Run each candidate prompt over the same sample through `inlay-text-engine` and score the result with `evaluateCorrection`. Record status, rejection reason and helper `elapsed`.
4. Score fallback rate, golden matches and prompt tokens. Judge the delivered text, and judge the baseline twice to see the noise.
5. For latency, run `inlay-engine` and `inlay-text-engine` on the archived `inference.wav` files and record each stage. Run one experiment at a time, and log the load average with each run.
6. Commit only the winning prompt or parameters. Then run `bun run generate:api --typescript-only`, `bun run check`, `bun run lint` and `bun run fmt:check`.

The golden expectations and transcripts are personal dictation, so they stay with the rig rather than in this repository.
