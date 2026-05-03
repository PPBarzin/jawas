# Jawas - Phase 2 - Hunter service

## Objectif

La phase 2 de jawas a pour objectif de chasser réellement les primes à la liquidation sur le protocole Kamino.

Toutes les possibilités de liquidation ne seront pas prises en compte et un filtre configurable dans le système sera intégré.

## Fonctionnement d'une liquidation - Principe théorique

Kamino est une plateforme de fourniture/emprunt. 
Un utilisateur peut fournir (supply) un token A sur kamino afin d'en récupérer des intérêt. 
Sur base du montant en dollar de cette supply, l'utilisateur peut emprunter un autre token B à raison du LTV. Le LTV est un calcul qui autorise l'emprunteur à emprunter jusqu'à une limite. Le LTV s'exprime en %. Par exemple, pour le couple de token A/B le LTV est à 60%, cela veut dire que l'emprunteur peut emprunter en token B 60% de la valeur de dollar de son token A.

Cependant, la valeur des tokens n'évoluant pas en même temps, ce seuil de LTV peut être dépassé et l'obligation (supply/borrow) passe en unhealty LTV. Ce qui signifie que le LTV de l'obligation a dépassé le seuil du LTV et l'obligation peut être liquidée.

**Mécanisme de liquidation**

Au moment de la détection de dépassement du seuil de unhealthy LTV, Kamino informe que l'obligation peut être liquidée. La course à la liquidation démarre. 
N'importe quel utilisateur peut liquider une partie de l'obligation.

1. Le bot de liquidation recoit un signal depuis leur RPC. 

2. Il analyse pour s'assurer que la liquidation entre dans leur critère (si bot institutionnel -> pas les petites liquidations)

3. S'il n'a pas le token mis en collatéral de l'obligation, il swap un token x vers le token mis en collatéral ou il fait un flash loan de ce token

4. Le bot envoie la transaction de liquidation

5. Kamino envoie une récompense correspondant à un pourcentage de la dette remboursée (kamino withdraw le collatéral à hauteur de X x (1+y%) en dollar

6. La récompense est immédiatement swappée vers un token liquide (comme USDC)

Note: les points 3 à 6 se font dans une seule transaction sur solana. Si une seule de ces transactions n'est pas réalisée, l'ensemble des transactions sont *FAILED*.

## Optimisation du process

Tout l'enjeu de ce process est d'être le plus rapide.
Plusieurs éléments jouent sur la rapidité bout à bout

1. La vitesse du noeud RPC choisit

2. La vitesse du bot

3. La complexité des transactions

### Vitesse du noeud RPC

Actuellement nous travaillons sur le noeud RPC Quicknode [Quicknode - Endpoints](https://dashboard.quicknode.com/endpoints) (login avec google)

| Provider   | Latence P95 (ms)                                                                    | Uptime SLA | Free Tier (req/mois) | Prix paid (entry) | Meilleur pour (bot/DeFi)                                                                                      |
| ---------- | ----------------------------------------------------------------------------------- | ---------- | -------------------- | ----------------- | ------------------------------------------------------------------------------------------------------------- |
| QuickNode  | 50 [blog.quicknode](https://blog.quicknode.com/solana-latency-benchmark-quicklee/)  | 99.95%     | 10M                  | $49/m (250 RPS)   | Latence globale, add-ons, MEV [blog.quicknode](https://blog.quicknode.com/solana-latency-benchmark-quicklee/) |
| Triton One | <100 [rpcfast](https://rpcfast.com/blog/rpc-node-providers-on-solana)               | 99.99%     | Non                  | $2900/m ded.      | Trading/bots ultra-low latency [dysnix](https://dysnix.com/blog/solana-node-providers)                        |
| Helius     | 225 [blog.quicknode](https://blog.quicknode.com/solana-latency-benchmark-quicklee/) | 99.99%     | 1M (10 RPS)          | $49/m             | APIs NFT/DeFi, webhooks, Geyser [sanctum](https://sanctum.so/blog/complete-guide-solana-rpc-providers-2026)   |
| Chainstack | 187 [blog.quicknode](https://blog.quicknode.com/solana-latency-benchmark-quicklee/) | 99.99%     | 3M (25 RPS)          | $99/m (250 RPS)   | ShredStream, SOC2, scale [chainstack](https://chainstack.com/best-solana-rpc-providers-in-2026/)              |
| Alchemy    | 237 [blog.quicknode](https://blog.quicknode.com/solana-latency-benchmark-quicklee/) | 99.9%      | 30M (généreux)       | $49/m             | Multi-chain, dev-friendly [alchemy](https://www.alchemy.com/overviews/solana-rpc)                             |
| RPC Fast   | <15 (EU/US) [rpcfast](https://rpcfast.com/blog/solana-rpc-node-full-guide)          | 99.99%     | Pay-as-you-go        | $2300/m ded.      | Bare-metal, failover auto [dysnix](https://dysnix.com/blog/solana-node-providers)                             |

QuickNode semble être le meilleur choix actuellement. Le plan free a un impact modéré sur la latence, passage à 232 ms.

Si on voit qu'on perd trop de transaction, le passage sur un plan payant pourrait nous faire gagner 182 ms. 

### Vitesse du bot

Le langage du bot sera rust. On estime une latence de 8 à 50 micro secondes.

### Les transactions

Comme vu plus haut, la complexité des transactions influent sur la vitesse. Plus une transaction est complexe plus, elle prendra du temps.
Vient à s'ajouter les fees. En effet, il est possible de proposer aux validateurs des fees de transactions pour mettre la transactions au dessus du panier. 

Donc, il y a deux volets:

- La complexité de la transaction

- Les fees associés à la transaction

**Complexité de la transaction**

La seule transaction nécessaire est celle de la liquidation. Les transactions annexes comme le flash loan, le swap, etc. sont des transactions permettant d'assurer de la viabilité économique de la liquidation. Nous appelerons ces transactions les *transactions secondaires* 
Nous devons donc nous concentrer pour supprimer ces transactions secondaires.

La première est le **flash loan ou le swap avant liquidation.**
Pour supprimer cette transaction, il faut de manière triviale posséder le token à repay dans son wallet. 
On ne peut pas posséder tous les tokens dans un wallet notamment parce qu'il existe des tokens exotiques qu'on ne voudrait pas avoir.
Une whitelist exhaustive des tokens qu'on pourrait avoir en wallet sera implémenté dans airtable. Cette whitelist sera constuite de manière à ce que les tokens référencés soient échangeables sur les marchés. C'est-à-dire qu'ils doivent pouvoir être swappé vers un stablecoin facilement.
Ensuite, nous allons vérifier depuis le site de kamino quels sont les obligations dont la probabilité de liquidité sont les plus grandes. Pour ce, le site: [Kamino loans analysis](https://risk.kamino.finance/Loans_Analysis_Realtime) nous donne en temps réel quelles sont les positions les plus susceptible d'être liquidée. Le site indique également quel est le token emprunté sur l'obligation.
Nous ferons un wallet qui possède uniquement les tokens trouvés sur ce site web mais qui sont également dans la white list.
Nous mettrons en wallet 5 à 10 tokens dépendant de la valeur des positions à liquider sur le site.

De cette manière, nous préparons en *off* un wallet avec 5 à 10 tokens prêt à être utilisé pour des futures transactions. Ces tokens seront liquides car dans la whitelist.

La deuxième transaction secondaire à supprimer est **le swap post liquidation**.

Celui ci est vraiment simple, on envoie la transaction de liquidation et on recoit le token liquidé. Ce token aura au préalable été filtré via notre whitelist et est donc, par hypothèse, liquide. 
La transaction de swap vers un stablecoin peut donc être faite après, la transaction principale de liquidation sans aucune difficulté ni risque et avec des fees de transactions bas.

**fee associé à la transaction**

Ce paramètre influe directement sur la performance financière du bot. Plus le fee est élevé, plus la performance financière du bot est faible. Et inversement.
Ce sera un paramètre de l'installation - fee_percentage_kamino. 
Ce paramètre exprime la proportion du profit estimé qui sera passé en fee.

fee = percent_estimated_profit_kamino x repay_amount x fee_percentage_kamino

le fee sera le produit d'un paramètre percent_estimated_profit_kamino par la quantité à repayer pour liquider (repay_amount) par le pourcentage de fee alloué à la transaction (fee_percentage_kamino)

Il est important que le calcul de profit sera fait sur base d'une estimation (percent_estimated_profit_kamino)
Cependant, afin d'avancer de manière sécurisée, au premier moment de vie de *Hunter service* , le paramètre fee_percentage_kamino sera standard à toute transaction sur Solana. Nous montrons en pourcentage si nous voyons trop d'obligations dont le bot n'a pas pu liquider l'obligation.

Un monitoring avec une score card devra être mis en place dans airtable.

## Systèmes mis en jeu

### jawas-hunter

Coeur du bot. Ce bot sera écrit en rust afin d'optimiser la vitesse d'exécution du bot.

Les obligations pouvant être liquidée seront séléctionnées en fonction des critères suivant:

* Valeur du repay inférieur à la valeur du token dans le wallet

* Valeur du repay maximun afin de ne pas entrer dans le terrain de jeu des bots institutionnels

* Valeur du repay minimal afin de ne pas entrer dans un bruit de repay

Chaque critère peut être activé ou désactiver dans le fichier de configuration config.yaml

### Airtable

Airtable sera l'outil d'enregistrement des logs pertinents.
On y mettra:

- Les liquidations réussies

- Les liquidations ratées

- Les liquidations non considérées

- les divers fees

- la whitelist

Dans tous les cas, le remplissage de airtable se fera **après** la routine de liquidation du bot afin de ne pas augmenter le temps d'exécution du bot.

**Tables prévue**:

* *Jawas-whitelist*: Whitelist des tokens

* *jawas-liquidation*: logs des liquidations réussies,ratées ou non considérées. Cette table doit inclure les fees de transactions pour calculer le rendement des liquidations.

* *jawas-wallet*: logs reprenant les différentes transactions fee inclus pour construire le wallet en vue de liquidation

* *parameters*: Table (existante) reprenant les paramètres ainsi que le bouton permettant d'envoyer un webhook. Les paramètres sont des fields.

# Whitelist

La whitelist sera dans Airtable. Les différents champs nécessaire à la construction du wallet seront intérgrés dans la table. (A définir ultérieurement).
La table est "jawas-whitelist"

Dans tous les cas, un flag "Active" sera intégré à la table afin de filter les tokens actifs.

La construction de la whitelist sera faite de manière asynchrone par l'utilisateur.

# Paramètres

Afin d'augmenter la vitesse du bot, les paramètres seront écrit dans le fichier config.yaml et reporté en lecture dans la base airtable au premier cycle du bot. (1x pour visualisation). En contre partie, tout changement de paramètre impliquera un rebuilt du container. Ceci est acceptable et doit être documenté car les changements de paramètres interviennent de manière indépendante de la transaction principale.

**Liste des paramètres**: 

* nombre_token: nombre de token maximum à preparer dans le wallet

* percent_estimated_profit_kamino: profit estimé sur kamino

* fee_percentage_kamino: pourcentage de fee acceptable pour la transaction de liquidation

* watchdog_time: temps de scrutation des transactions permettant de détecter un RPC non communiquant

**Note au sujet des paramètres**

- Toutes les variables liées à une plateforme (ex. kamino) auront dans leur nom, le suffixe "_kamino"

- Les paramètres inclus dans le fichier config.yaml seront uniquement ceux qui servent à la transaction principales, les autres seront dans airtable

# Fonctionnalité principale

La fonctionnalité principale est donc assez linéaire:

- On recoit via le RPC une possibilité de liquidation

- On vérifie si la liquidation entre dans nos critères

- On lance la liquidation

- Quand on recoit le statut de la transactions (FAILED ou PASSED), on loggue dans Airtable dans la table *jawas-liquidation*.

- Envoie d'un webhook pour la reconstruction du wallet (hors transaction principale)

# Fonctionnalités secondaires

### Swap de fin de liquidation

En fin de liquidation, un swap vers le token repay peut être réalisé (si fonctionnalité activée). Sinon le token reste dans le wallet.

## Reconnexion au RPC automatique

Comme vu lors de la phase 1, nous avons pu voir une déconnexion non détecté au RPC. Il est indispensable de créer un système permettant de détecter une déconnexion et faire une connexion automatique.
Actuellement, si nous ne recevons pas de transactions depuis plus de 120 secondes, le bot considère qu'il est aveugle et relance la connexion.

## Construction du wallet

Comme vu plus haut et afin de diminuer le temps d'exécution du bot, la construction du wallet se fera hors des transactions principales et secondaires. La construction du wallet ne doit pas venir prendre de la bande passante au système. La construction du wallet se fait sur base d'évenement. 

Evenement lancant la construction du wallet:

- En fin de liquidation

- Au démarrage du bot

- Sur un webhook recu de Airtable

Chaque evenement sera activable depuis des paramètres intégrés dans airtable.

**Mécanisme de construction du wallet**

1. Depuis le site [Kamino Loans_Analysis_Realtime](https://risk.kamino.finance/Loans_Analysis_Realtime), on récupère l'ensemble des obligations dont la valeur "Dist To Liq" est inférieure ou égale à 0.1 (paramètre à ajouter dans airtable)

2. On ne garde que les obligations dont les tokens (borrow et deposit) font partie de la whitelist. (c'est un AND le token borrow AND le token deposit font partie de la whiteliste) et dont les conditions de liquidations sont rencontrées (ie. valeur du repay)

3. On calcul la proportion de chaque token sur l'ensemble des positions

4. On construit le portefeuille sur base de cette proportion. Il faut donc regarder quel token est en excès dans le wallet et le swaper vers le token qui est en déficit.

### Sécurisation des bénéfices

Un programme séparé devra permettre de sécuriser les bénéfices depuis les différents token du wallet vers de l'USDC.
Cette sécurisation fonctionne sur base d'un webhook recu depuis Airtable. 

### Note d'architecture

# Retour Senior Dev : Analyse Critique & Risques

L'approche de la Phase 2 est solide, notamment sur l'optimisation des **Compute Units (CU)** via l'inventaire. Cependant, pour passer d'un prototype à un bot de production rentable, voici les points de friction critiques à anticiper :

### 1. Le Risque d'Inventaire (Le "War Chest Trap")

Posséder les tokens (SOL, USDC, etc.) permet d'être plus rapide qu'un Flash Loan, mais expose le bot à la volatilité.

* **Risque :** Les liquidations se produisent lors des krachs. Si le bot détient 10 SOL pour liquider une position et que le SOL perd 15% en 10 minutes, le profit de la liquidation (souvent 5-10%) ne couvrira pas la perte de valeur du capital immobilisé.
* **Action :** Implémenter une règle de "Stock Tournant". L'inventaire ne doit être constitué que si une opportunité est imminente. Sinon, le capital doit dormir en USDC.

### 2. L'Avantage Concurrentiel Réel (CU vs Niche)

L'idée que les bots institutionnels ignorent les "petites liquidations" est un mythe partiel. S'ils sont rentables, ils les prendront.

* **La Vraie Victoire :** Une transaction avec Flash Loan + Swap consomme ~300k CU. Une transaction "Surgical" (uniquement l'instruction `Liquidate`) consomme ~50k CU. 
* **Stratégie :** En consommant 6x moins de ressources, Jawas peut proposer un **Priority Fee par CU** beaucoup plus agressif que les institutions tout en restant plus rentable sur les petits montants ($50-$200). C'est là que réside ton "unfair advantage".

### 3. Latence des Sources de Données (Oracle vs Dashboard)

Dépendre du site `risk.kamino.finance` pour préparer le wallet est dangereux. 

* **Risque :** Un dashboard web a souvent 5 à 30 secondes de retard sur la blockchain. Les bots concurrents calculent la "Distance to Liquidation" en écoutant directement les **Oracle Price Feeds (Pyth/Switchboard)**.
* **Action :** À terme, Jawas devra intégrer son propre moteur de calcul de santé des obligations basé sur les prix Oracles pour "voir" l'opportunité avant qu'elle n'apparaisse sur le dashboard Kamino.

### 4. Gestion des Fees et Jito Bundles

Le calcul des fees en pourcentage du profit est une approche Ethereum. Sur Solana, c'est une guerre de priorité.

* **Action :** L'utilisation de **Jito Bundles** (pourboire au validateur) est impérative en Phase 2. Cela permet d'envoyer une transaction qui n'est exécutée (et payée) que si elle réussit. Cela évite de brûler des frais pour des transactions qui arrivent 1ms trop tard.

### 5. Latence de Configuration (Airtable vs Config.yaml)

* **Risque :** Airtable est parfait pour le monitoring, mais trop lent pour la configuration temps réel (latence API HTTP). 
* **Action :** Tous les paramètres influençant la décision de transaction (Min Profit, Max Fee, Whitelist) doivent être chargés en mémoire locale au démarrage (ou via un refresh périodique asynchrone) pour ne pas ralentir la boucle de détection.

---

*Note : Ce retour a été intégré pour orienter le développement du `HunterService` vers une architecture "Low-Latency / High-Precision".*
