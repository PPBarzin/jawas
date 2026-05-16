# Analysis

Ce dossier sert a conserver, par cas de `HUNTER_FIRING`, les donnees brutes et l'analyse.

## Structure

- `kamino-firing/README.md`
  - index des cas connus
- `kamino-firing/<case>.md`
  - fiche de travail d'un `FIRING`

## Sources de donnees

Pour chaque cas, on veut idealement conserver:

1. La ligne Airtable `HUNTER_FIRING`
2. La ligne Airtable associee:
   - `HUNTER_BUNDLE_SENT`
   - ou `HUNTER_BUNDLE_FAILED`
3. Les extraits prod de:
   - `hunter_trace.jsonl`
   - `hunter_signal_metrics.jsonl`
   - `docker logs jawas-kamino`

## Convention

Une fiche de cas doit contenir:

- un resume
- les raw data connues
- les artefacts manquants
- une analyse courte
- une conclusion provisoire
