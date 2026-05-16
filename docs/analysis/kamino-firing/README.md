# Kamino Firing Cases

Cas identifies pour l'instant a partir de [Specification/Jawas-Watch-kamino work.csv](/home/ppbarzin/Documents/Programmation/tools/Jawas/Specification/Jawas-Watch-kamino%20work.csv):

1. [2026-04-22T23-25-27Z-4JxUtqhP.md](/home/ppbarzin/Documents/Programmation/tools/Jawas/analysis/kamino-firing/2026-04-22T23-25-27Z-4JxUtqhP.md)
2. [2026-04-23T00-11-08Z-24Sfd1TQ.md](/home/ppbarzin/Documents/Programmation/tools/Jawas/analysis/kamino-firing/2026-04-23T00-11-08Z-24Sfd1TQ.md)
3. [2026-04-23T00-11-09Z-5tYDieYJ.md](/home/ppbarzin/Documents/Programmation/tools/Jawas/analysis/kamino-firing/2026-04-23T00-11-09Z-5tYDieYJ.md)

## Commandes prod a extraire pour chaque cas

Remplacer `<SIG>` par la signature du cas.

```bash
docker logs jawas-kamino 2>&1 | grep '<SIG>'
docker exec -it jawas-kamino sh -lc "grep '<SIG>' hunter_trace.jsonl"
docker exec -it jawas-kamino sh -lc "grep '<SIG>' hunter_signal_metrics.jsonl"
```

Si `grep` brut retourne trop de bruit, utilise une fenetre de temps:

```bash
docker logs jawas-kamino --since 2026-04-22T23:24:30Z --until 2026-04-22T23:26:30Z 2>&1
```
