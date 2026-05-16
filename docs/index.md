# Jawas

Les bots de liquidation réactifs sur Solana arrivent structurellement trop tard.

**Jawas** est un projet de recherche expérimental qui cherche à comprendre pourquoi.

Ce n'est pas un logiciel financier prêt pour la production.

---

## Ce que Jawas fait

Jawas expose deux modes d'exécution :

- **Observer** — s'abonne aux logs de protocole, enrichit les événements de liquidation et écrit des données de recherche
- **Hunter** — écoute les signaux de liquidation et tente un pipeline de réaction pour Kamino ou Solend

Pour Kamino, la direction P1 actuelle est explicite dans le runtime :

- les signaux de liquidation observés peuvent alimenter une petite shortlist proactive
- seules les obligations dont le `repay mint` est déjà dans le wallet sont éligibles
- le firing peut être piloté par l'état shortlist de Hermes (modes `hybrid`/`only`), les signaux réactifs étant conservés pour l'observabilité

---

## Ce qui ne fonctionne pas encore bien

- Le hunter reste majoritairement réactif et donc souvent second dans les situations compétitives.
- `bundle_sent` n'est pas équivalent à une liquidation confirmée. Cela signifie uniquement que le endpoint Jito a accepté la soumission.
- La couverture du wallet est volontairement étroite — certaines opportunités sont ignorées par design.
- La qualité RPC, le délai de propagation et la stratégie de frais Jito dominent les résultats plus que l'élégance du code.

---

## Hypothèses principales

- L'observation est utile même quand l'exécution n'est pas encore compétitive.
- Le goulot d'étranglement est moins "peut-on envoyer une transaction de liquidation ?" et plus "peut-on savoir quoi envoyer avant que le gagnant soit visible on-chain ?"
- Une shortlist wallet-first et un état préchargé sont des prérequis pour aller au-delà de l'exécution réactive.

---

## Navigation

| Section | Contenu |
|---------|---------|
| [Concepts](specifications/rpc_wss_helius_quicknode_explained.md) | RPC, Jito, Hermes — explications de fond |
| [Architecture](architecture.md) | Design du projet et flux runtime |
| [Spécifications](specifications/index.md) | Specs de features et contrats de comportement |
| [Opérations](lessons-learned/index.md) | Leçons apprises en production |
| [Recherche](research-notes.md) | Findings actuels et directions |
| [Analyse](analysis/index.md) | Rapports datés et cas d'étude |
