# Jawas - Fonctionnement de la production

> Statut : note technique de partage interne  
> Date : 2026-05-11  
> Objet : décrire le fonctionnement réel de Jawas en production, pas le workflow théorique idéal

---

## 1. Intention

Jawas en production n'est pas un moteur de liquidation "pré-calculé" qui connaît toutes les obligations à risque à l'avance.

Dans son état actuel, Jawas reste principalement un système **réactif** :

- il écoute des signaux on-chain
- il tente de qualifier rapidement une cible
- il construit une transaction de liquidation
- il l'envoie via Jito / RPC

Le point important pour la lecture de cette note :

> le système prod sait maintenant construire un chemin de liquidation Kamino beaucoup plus propre qu'avant,  
> mais il reste dépendant d'un signal souvent tardif.

---

## 2. Composants runtime

La production Jawas repose sur trois blocs principaux.

### 2.1 Observer

Rôle :

- écouter les programmes ciblés via WebSocket
- détecter les transactions / logs liés aux liquidations
- enrichir les événements avec des lectures RPC
- persister l'observation dans Airtable et les traces JSONL

Ce bloc sert surtout à :

- comprendre ce qui s'est passé
- estimer la concurrence
- mesurer le délai de réaction

### 2.2 Hunter

Rôle :

- écouter des signaux protocolaires
- décider si une cible vaut un tir
- appliquer les contraintes wallet / whitelist
- construire le bundle ou la transaction de liquidation
- l'envoyer

Le hunter est le chemin critique de production.

Point de fonctionnement explicite :

- le hunter ne démarre pas si `wallet.toml` est absent ou illisible
- sa configuration runtime reste relue à chaque redémarrage de boucle, ce qui permet d'appliquer des ajustements d'env sans redémarrer tout le process
- ce reload de config se fait volontairement au point de redémarrage de boucle du hunter ; ce n'est pas un effet de bord implicite

### 2.3 Heartbeat / logs

Rôle :

- signaler que le bot tourne
- fournir de la matière de diagnostic
- garder une trace fine des étapes `ws_received -> firing -> bundle_sent -> ...`

Les traces importantes sont notamment :

- `hunter_trace.jsonl`
- `hunter_signal_metrics.jsonl`
- Airtable `Jawas-Watch`

Le logger Airtable fonctionne maintenant avec un worker explicite démarré depuis `app.rs` :

- `AirtableLoggerAdapter::new(...)` ne spawn plus de tâche cachée
- le runtime construit le logger
- puis démarre explicitement son worker de flush / batch

---

## 3. Flux production simplifié

Le fonctionnement actuel peut se résumer ainsi :

1. Jawas reçoit un signal on-chain sur Kamino.
2. Le hunter résout l'obligation ciblée.
3. Il vérifie les contraintes locales :
   - token de repay couvert par `wallet.toml`
   - cap de repay non nul
   - contexte de wallet disponible
4. Il prépare le contexte d'exécution Kamino.
5. Il construit la transaction.
6. Il l'envoie via Jito / RPC.
7. Il journalise l'étape atteinte.

Le point subtil est que l'étape 4 n'est plus seulement "prendre un dépôt et un borrow au hasard".

Le chemin prod actuel a été renforcé pour :

- refresh toutes les reserves actives connues de l'obligation
- construire `RefreshObligation` avec le contexte complet quand il est disponible
- utiliser les `token_program` dérivés des reserves
- créer les ATA de destination de manière idempotente si nécessaire
- retirer ces ATA additionnelles si la taille de la tx devient trop grande

---

## 4. Ce que la production fait aujourd'hui sur Kamino

### 4.1 Réception du signal

Le hunter reçoit un signal Kamino via logs / WebSocket.

Ce signal n'est pas une preuve que l'obligation est encore libre.
Il signifie surtout :

- qu'une activité pertinente a été observée
- qu'une liquidation est peut-être en cours
- qu'il faut décider vite si une tentative vaut encore la peine

### 4.2 Shortlist et contexte préparé

Jawas maintient un mécanisme de shortlist / contexte préparé.

Son rôle est de stocker, avant le tir, des informations réutilisables :

- obligation cible
- reserves actives
- éléments utiles à la construction

L'objectif est de déplacer une partie du travail **hors** du moment critique.

### 4.3 Contraintes wallet

Le hunter ne peut pas repayer n'importe quel actif.

Il ne tente la liquidation que si le mint de repay :

- est présent dans `wallet.toml`
- possède un `max_repay_native > 0`
- correspond à un token réellement couvert par le wallet du liquidateur

En pratique, cela exclut automatiquement :

- les mints non whitelistés
- les mints à cap nul
- les cas où le wallet n'est pas prêt

L'absence ou l'illisibilité de `wallet.toml` est maintenant traitée comme une erreur bloquante de runtime, pas comme un simple warning.

### 4.4 Construction de la transaction

Le chemin Kamino en production comprend maintenant :

1. `RefreshReserve` pour les reserves actives connues
2. `RefreshObligation`
3. création idempotente des ATA destination si utile
4. `LiquidateObligationAndRedeemReserveCollateralV2`
5. encapsulation dans un bundle / un envoi RPC avec compute budget et tip

### 4.5 Envoi

Le hunter peut :

- envoyer via Jito
- ou via RPC selon le mode et le contexte

En production, `bundle_sent` signifie :

- le block engine a accepté la requête `sendBundle`

Cela **ne prouve pas** :

- que la transaction a été incluse
- ni qu'elle a gagné la course

---

## 5. Logs réellement utiles

Pour comprendre ce que le bot a fait, les champs les plus utiles côté hunter sont :

- `ws_received`
- `signal_accepted`
- `signal_rejected_duplicate`
- `skip`
- `firing`
- `dry_run`
- `bundle_retry`
- `bundle_sent`
- `bundle_send_failed`

Depuis les derniers correctifs Kamino, les champs de contexte suivants sont particulièrement utiles :

- `active_reserve_count`
- `full_refresh_context`
- `tx_size_bytes`
- `ata_setup_instruction_count`
- `ata_setup_dropped_for_size`

Ils permettent de distinguer :

- un tir avec contexte incomplet
- un tir avec contexte complet
- un tir potentiellement trop gros
- un tir nécessitant des comptes ATA supplémentaires

---

## 6. Ce que la production ne fait pas encore

Il est important de dire explicitement ce que Jawas prod **ne fait pas** encore.

### 6.1 Il n'anticipe pas encore assez tôt

Jawas reçoit encore très souvent un signal déjà tardif.

Concrètement :

- l'obligation est parfois déjà redevenue `healthy`
- ou un autre liquidateur a déjà consommé la fenêtre

### 6.2 Il ne prouve pas encore le destin complet du bundle

Aujourd'hui, la télémétrie montre bien :

- `bundle_sent`

mais pas encore de manière robuste :

- bundle landed
- bundle dropped
- bundle expired
- transaction incluse mais failed

### 6.3 Il ne fait pas une simulation complète sur le hot path prod

C'est volontaire pour la vitesse.

Le coût :

- certaines opportunités peuvent être filtrées tard
- on n'a pas la même qualité d'information qu'un probe offline

### 6.4 Il reste un système réactif

Même avec un bon wiring de transaction, le système prod reste largement dépendant de :

- la qualité du signal
- la vitesse du provider
- la vitesse de traitement
- et le fait que la fenêtre soit encore ouverte

---

## 7. Ce que les observations récentes ont montré

Les observations récentes sur Kamino conduisent à une lecture simple :

1. Le dernier tronçon technique a été amélioré.
2. Le bot sait construire un tir Kamino plus crédible qu'avant.
3. Mais cela ne suffit pas à garantir un succès si la fenêtre utile est déjà consommée.

Un cas réel sur obligation de test a montré :

- un snapshot brut devenant liquidatable
- puis un état on-chain déjà nettoyé quand le watcher a pu simuler utilement

Ce point est important pour les collègues qui liront cette note :

> le système n'échoue pas seulement parce qu'il construirait mal sa transaction,  
> il échoue aussi parce que le marché et les autres liquidateurs agissent avant que notre voie réactive puisse se transformer en gain.

---

## 8. Lecture correcte du problème actuel

La mauvaise lecture serait :

> "Le bot ne sait pas liquider."

La lecture plus juste est :

> "Le bot sait de mieux en mieux tirer, mais il reste trop souvent en retard sur le moment où tirer devient encore utile."

Autrement dit :

- le problème purement technique du dernier maillon a diminué
- le problème structurel de temporalité reste dominant

---

## 9. Question ouverte pour la suite

La question prioritaire n'est plus seulement :

> "Comment construire la liquidation ?"

Elle devient de plus en plus :

> "Comment disposer d'un état déjà préparé avant que la liquidation observable ne soit déjà en train d'être gagnée par un autre ?"

Cela pousse naturellement la recherche vers :

- la préparation proactive des obligations proches du seuil
- des signaux plus précoces
- des shortlist persistantes et immédiatement activables
- des heuristiques qui détectent l'approche d'une liquidation avant le signal public le plus évident

---

## 10. Résumé en une phrase

Jawas en production fonctionne aujourd'hui comme un **liquidateur réactif techniquement plus propre qu'avant, mais encore trop souvent tardif au moment où la fenêtre de liquidation utile existe réellement**.

---

## 11. Orientation Phase 3

La Phase 2 a surtout servi à :

- reprendre la dette technique
- fiabiliser le dernier tronçon Kamino
- mesurer honnêtement où Jawas perd

La Phase 3 ne doit plus être lue comme une extension naturelle du même modèle réactif.
Elle correspond à un changement d'objectif :

> ne plus seulement comprendre la course,
> mais commencer à la gagner.

### 11.1 Première brique prioritaire : Pyth / Hermes

Le premier investissement Phase 3 retenu est l'intégration de `Pyth/Hermes`.

Pourquoi :

- le signal prix est disponible plus en amont que la liquidation observable
- l'intégration peut commencer en lecture seule
- le coût d'entrée est faible, voire nul selon le mode d'accès retenu
- cela permet de recalculer localement la santé des obligations avant le signal réactif actuel

Autrement dit, `Hermes` est traité comme la première brique de `jawas-liquidator`, pas comme un simple enrichissement périphérique.

### 11.2 Deuxième brique : Yellowstone / Triton

La deuxième piste Phase 3 retenue est un prototype `Yellowstone gRPC` avec `Triton`.

Objectif :

- disposer d'un flux comptes / oracles plus proche du validator
- réduire le délai entre le changement d'état utile et sa réception locale
- tester un `shadow state` Kamino plus crédible que le pipeline WebSocket actuel

Point pratique important :

- Triton propose un mode `Custom PAYG`
- avec un dépôt minimum de `125 USD`
- valide `12 mois`

Cela rend le prototype plus accessible qu'un saut immédiat vers une infrastructure dédiée lourde.

### 11.3 Helius est conservé

Helius n'est pas retiré de la stratégie.

Il reste utile pour :

- l'observation
- les logs
- le diagnostic
- les lectures de support
- certaines intégrations pratiques côté développeur

En Phase 3, Helius doit être vu comme une couche de support utile, pas comme l'unique source supposée gagner la course.

### 11.4 QuickNode sort du hot path

L'orientation Phase 3 retenue est de simplifier la hiérarchie des providers :

- `Pyth/Hermes` pour la composante prix amont
- `Triton/Yellowstone` pour le flux compétitif à prototyper
- `Helius` pour le support et l'observabilité

Dans cette lecture, `QuickNode` n'a plus vocation à rester dans le hot path compétitif.

### 11.5 Pré-requis transversal

Avant d'engager pleinement ces briques Phase 3, une revue de qualité du code reste un pré-requis raisonnable :

- structure
- lisibilité
- fragilité
- testabilité
- dette technique non algorithmique

L'objectif n'est pas de ralentir la transition, mais d'éviter qu'un bot plus agressif repose sur une base logicielle trop fragile.
