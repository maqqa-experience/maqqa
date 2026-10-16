# Third-party

Optional upstream checkouts live here and are **not** vendored in git.

| Path | Source | Used by |
|------|--------|---------|
| `LeetCodeDataset/` | [newfacade/LeetCodeDataset](https://github.com/newfacade/LeetCodeDataset) (MIT) | `scripts/import_leetcode.py` → slim JSONL under `fixtures/leetcode/` |

```bash
python3 scripts/import_leetcode.py --clone
```
