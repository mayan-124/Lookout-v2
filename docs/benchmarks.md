# Benchmarks

Measured numbers copied from `docs/decisions.md`, not re-measured for Phase 6. Machine: i5-12400F (6 cores, 12 threads), NVMe, Windows 11. Generated data only; real folders differ (longer names, colder disks). Details, caveats and the other phases' numbers are in `decisions.md`.

## File-name search (1,000,000 generated names)

| measure | result |
|---|---|
| index search p95 | 0.53 to 6.36 ms |
| keystroke to first results through the engine, p95 | 0.86 to 6.54 ms (max 7.36); the UI hop is not included |
| first-results p95 by thread count, worst query `a` | 8 threads 10.53 ms, 4 threads 11.89, 1 thread 38.29 |
| idle memory, combined private working set, 3 runs | 126.1, 126.0, 125.9 MB (budget 150) |
| memory per name | about 49 bytes |
| crawl of 1M files, unthrottled / at 10% duty | 8.2 s / 42.2 s |
| peak memory during a rescan | +37.8 MB over idle (estimate for the full app 153 to 173 MB, over budget, so scheduled rescans are off by default) |
| watcher burst, 100,000 new files | 0 missed |

## Content search and OCR

- Idle memory with the 1M-name index and full-text reader: 126.3, 126.0, 126.3 MB.
- Content and OCR batch (3,000 text files, 60 screenshots, 2 runs): peak 94.1 and 95.1 MB, 85.6 and 86.8 MB at 120 s; the batch took about 62 s.
- Measured on a few thousand documents, not at the million scale.

## Semantic search ("By meaning")

Generated fixtures only: 60 files (48 notes, 12 rendered screenshots), then 2,060 with 2,000 generated distractors. 50 queries written before tuning (30 tune, 20 holdout). Threshold 0.60.

| 2,060 files | holdout 20 top-1 / top-5 | all 50 top-1 / top-5 |
|---|---|---|
| existing content search (all words must match) | 0 / 0 | 0 / 0 |
| fair keyword baseline (BM25, stemming) | 6 / 11 | 14 / 21 |
| keyword + semantic | 12 / 14 | 28 / 33 |

These queries were written to describe content in other words, so they favour semantic search; 17 of 50 still miss the top 5. With the distractors, off-topic queries return results at 0.60 (4 of 4).

| measure | result |
|---|---|
| int8 against fp32 vectors, mean cosine | 0.9966 (min 0.9937) |
| index of 2,060 files | 21.9 s at full speed |
| `lookout.exe` memory, plugin on at idle / off | 9.5 / 6.9 MB (about +2.6 MB; single runs) |
| helper private working set | about 64 MB indexing, 62 MB warm (cap 1 GiB) |
| 100,000 random vectors: `vectors.log` | 41.3 MB, loads in 170 ms |
| brute-force scan, 100,000 vectors | 9.3 ms median (11.2 max) |
| cold first query / warm query, 100,000 vectors | 484 ms / 13 ms median |
| helper working set with 100,000 vectors | 113.5 to 119.8 MB |

Not measured: embeddings against the reference `sentence-transformers` implementation; real-world (non-generated) corpora; the installer and a clean PC.
