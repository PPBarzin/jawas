<img src="https://r2cdn.perplexity.ai/pplx-full-logo-primary-dark%402x.png" style="height:64px;margin-right:32px"/>

# Hello Perplexity,

je suis en train de construire un outil de liquidation sur Solana.
Quasiment tout fonctionne par contre, j'ai un soucis.

quand je vois une position qui serait intéressante à liquider, systématiquement, un autre liquidateur va plus vite que moi. Pas à cause de la vitesse de mon bot mais parce qu'il recoit l'information plus tôt.

Même en vérifiant un obligation spécifique, je me fais batte.

Je te met en annexe un rapport décrivant le fonctionnement de notre bot.

Peux tu regarder ce qui ne va pas? Quels sont les principales causes qui font que nous recevons l'information de liquidation trop tard?

J'ai maintenant une analyse complète du problème. Voici mon diagnostic détaillé basé sur le rapport de Jawas et les meilleures pratiques actuelles pour les bots de liquidation sur Solana.

***

## Diagnostic : pourquoi Jawas reçoit le signal trop tard

Le rapport de Jawas lui-même identifie la cause racine  : *"le système prod reste dépendant d'un signal souvent tardif"*. Voici une analyse structurée des 4 niveaux du problème, du plus critique au moins critique.[^1]

***

## 1. Le signal WebSocket est structurellement tardif

C'est **la cause principale**. Jawas écoute les programmes Kamino via WebSocket classique. C'est une erreur d'architecture fondamentale pour du MEV compétitif.[^1]

Le WebSocket RPC Solana introduit **200 à 500 ms de latence** après confirmation d'un slot, car il passe par la file d'événements interne du nœud RPC, la sérialisation JSON, et n'est pas connecté directement au pipeline Geyser du validateur. En comparaison :[^2]


| Méthode | Latence typique | Verdict pour liquidation |
| :-- | :-- | :-- |
| Solana Shreds (UDP) | ~0.02 ms | HFT, arbitrage agressif |
| **gRPC Yellowstone (Geyser)** | **10–50 ms** | ✅ Standard compétitif |
| gRPC filtré (ex. Subglow) | < 5 ms | ✅ Optimal |
| WebSocket RPC | 200–500 ms | ❌ Trop lent |
| RPC polling | 2–5 s | ❌ Hors-jeu |

[^3][^2]

Un concurrent qui utilise un flux **gRPC Yellowstone (Geyser)** voit les mises à jour de compte directement depuis la mémoire du validateur, **10 à 50 fois plus vite** que Jawas. Il est donc mécaniquement impossible de le battre avec du WebSocket.[^3]

***

## 2. La source du signal est réactive, pas prédictive

Jawas attend qu'un signal "activité pertinente" soit émis on-chain avant d'agir. Les liquidateurs les plus compétitifs n'attendent pas ce signal — ils **calculent eux-mêmes** en temps réel quel est le health factor de chaque obligation.[^1]

La bonne architecture proactive fonctionne ainsi  :[^4][^5]

- Abonnement gRPC aux **comptes oracle Pyth/Switchboard/Scope** → chaque mise à jour de prix est traitée immédiatement
- Abonnement gRPC aux **comptes d'obligation Kamino** → chaque changement de balance est traité immédiatement
- Recalcul du health factor local dès réception de l'un ou l'autre événement
- **Tir immédiat si health < 1.0**, sans attendre un signal protocolaire

La shortlist de Jawas va dans la bonne direction, mais elle reste déclenchée par un signal tardif. Il faut l'inverser : **construire en permanence la shortlist** à partir des prix et des positions, et tirer dès que le health factor passe le seuil.[^1]

***

## 3. Le signal écouté est l'oracle Scope de Kamino, pas Pyth directement

Kamino utilise **Scope**, son propre agrégateur d'oracles on-chain. Scope recopie Pyth/Switchboard dans un compte unifié et applique ses propres règles de validation. Il existe donc **deux étapes de propagation** avant que le changement de prix atteigne l'état de l'obligation :[^6][^7]

1. Pyth publie un nouveau prix → compte Pyth mis à jour on-chain
2. Scope lit Pyth et met à jour son price feed → compte Scope mis à jour
3. Le health factor de l'obligation change (si quelqu'un appelle `RefreshObligation`)
4. Jawas observe l'activité résultante

Si Jawas lit directement les **comptes Pyth** (ou les comptes Scope) via gRPC, il peut recalculer le health factor avant même que l'obligation soit officiellement marquée comme liquidatable. C'est ainsi que les concurrents avancés fonctionnent.[^4]

***

## 4. L'absence de preuve du destin du bundle = boucle de feedback aveugle

Le rapport mentionne que `bundle_sent` ne prouve pas l'inclusion. Ce n'est pas une cause de latence à la réception, mais cela empêche d'**optimiser le tip Jito** efficacement. Sans savoir si le bundle a *landed* ou *dropped*, il est impossible de calibrer le tip pour battre les concurrents à l'inclusion. Un tip trop bas → le bundle est accepté par le block engine mais jamais inclus.[^8][^1]

***

## Plan d'action prioritaire

**Court terme — impact immédiat :**

1. **Migrer vers gRPC Yellowstone** (Helius, Triton, ou node propre) pour le flux de données du Hunter. Gain attendu : 150 à 400 ms[^9][^2]
2. **Souscrire directement aux comptes Scope/Pyth** en plus des comptes d'obligation, pour recalculer localement le health factor dès un changement de prix, sans dépendre d'un signal protocolaire[^4]

**Moyen terme — avantage structurel :**
3. **Transformer la shortlist en moteur proactif** : maintenir un dictionnaire `obligation → health_factor` recalculé en continu ; déclencher le tir dès le passage du seuil, pas sur signal externe
4. **Ajouter un feedback sur le destin des bundles Jito** via polling `getBundleStatuses` pour optimiser le tip et savoir si des tirs sont *dropped* ou *landed*
5. **Colocaliser l'infrastructure** avec les validateurs Jito (serveur dans le même datacenter que les leaders de stake élevé) pour réduire la latence réseau sur la soumission[^10]

Le goulot d'étranglement de Jawas n'est pas dans la construction de la transaction (qui est correcte ) — il est entièrement dans la couche d'acquisition du signal. Passer du WebSocket au gRPC Geyser est la modification la plus impactante possible à court terme.[^3][^1]
<span style="display:none">[^11][^12][^13][^14][^15][^16][^17][^18][^19][^20][^21][^22][^23][^24][^25][^26][^27][^28][^29][^30]</span>

<div align="center">⁂</div>

[^1]: jawas_production_runtime.md

[^2]: https://subglow.io/solana-grpc-vs-rpc

[^3]: https://allenhark.com/blog/shreds-vs-grpc-vs-rpc-websockets

[^4]: https://subglow.io/use-cases/liquidation-bots

[^5]: https://baransel.dev/post/build-solana-liquidation-bot-that-makes-money/

[^6]: https://github.com/Kamino-Finance/scope

[^7]: https://github.com/hubbleprotocol/scope

[^8]: https://www.observers.com/solana-leaps-ahead-in-mev-race-by-making-transaction-ordering-transparent/

[^9]: https://dysnix.com/blog/solana-rpc-for-mev

[^10]: https://rpcfast.com/blog/solana-trading-co-location-low-latency

[^11]: https://kahawatungu.com/mev-and-trading-bots-on-solana-the-infrastructure-edge-that-decides-who-wins/

[^12]: https://github.com/openclaw/clawhub/issues/485

[^13]: https://blog.solend.fi/response-to-solana-network-issues-5c8184607283?gi=177eeebc9365

[^14]: https://lobehub.com/de/skills/sendaifun-skills-kamino

[^15]: https://sanj.dev/post/solana-mev-jito-deep-dive

[^16]: https://beincrypto.com/latest-solana-network-outage-arbitrage-bot-spam/

[^17]: https://kamino.com/docs/risk/kraf-dashboard

[^18]: https://kamino.com/security

[^19]: https://www.gate.com/learn/articles/solana-mev-an-introduction/2270

[^20]: https://www.reddit.com/r/solana/comments/1pdak2q/1480_people_got_liquidated_on_kamino_last_month/

[^21]: https://solanacompass.com/learn/accelerate-25/scale-or-die-at-accelerate-2025-the-state-of-solana-mev

[^22]: https://github.com/marinade-finance/scope

[^23]: https://subglow.io/best-solana-copy-trading-rpc

[^24]: https://colosseum.com/agent-hackathon/forum/279

[^25]: https://solana.com/docs/rpc/websocket

[^26]: https://www.gate.com/learn/articles/solana-sol-in-depth-research-an-emerging-power-in-the-blockchain-space/7996

[^27]: https://resources.cryptocompare.com/asset-management/14171/1714467000191.pdf

[^28]: https://docs.solanavibestation.com/developers/solana-rpc/websocket-methods/accountsubscribe

[^29]: https://github.com/Matt-Aurora-Ventures/Jarvis

[^30]: https://github.com/Elixir-Games-XYZ/scope

