# Comprendre Hermes dans Jawas

> Statut : note pédagogique
>
> Date : 2026-05-14
>
> Public visé : mainteneur de Jawas qui veut comprendre ce que fait Hermes dans le runtime Kamino, sans repartir directement du code Rust

---

## 1. L'idée simple avant tout

Dans Jawas, **Hermes n'est pas le moteur qui liquidate tout seul par magie**.

Hermes sert surtout à faire une chose très précise :

> **repérer à l'avance un petit sous-ensemble d'obligations déjà dangereuses**
>
> **écouter ensuite les flux prix Pyth en temps réel**
>
> **et déclencher un tir rapide quand une obligation shortlistée entre dans la fenêtre de liquidation**

Autrement dit :

- le vieux problème de Jawas était souvent : "on découvre l'obligation trop tard"
- Hermes essaie de transformer ça en : "on connaît déjà les candidats avant que le dernier mouvement de prix arrive"

La phrase utile à retenir est donc :

> **Hermes n'est pas un observateur global de toute la chaîne**
>
> **Hermes est un mécanisme de préparation + armement sur un petit ensemble de candidats**

---

## 2. Le problème que Hermes essaie de résoudre

Sans shortlist proactive, le bot réagit souvent comme ça :

1. un signal arrive
2. le bot découvre alors l'obligation
3. il recharge beaucoup de contexte
4. il résout les réserves actives
5. il construit la transaction
6. il envoie

Ce mode est fragile quand la concurrence est très rapide.

Avec Hermes, l'idée est différente :

1. on refresh périodiquement les obligations Kamino
2. on garde seulement les plus proches de la liquidation
3. on garde leur contexte préparé en mémoire
4. on écoute les updates de prix des feeds liés à ces obligations
5. quand un feed bouge dans le mauvais sens, on a déjà presque tout prêt

Donc Hermes n'est pas d'abord un sujet de "provider".
C'est d'abord un sujet de **timing informationnel** et de **préparation locale**.

---

## 3. Les deux composants Hermes

Dans le runtime, il faut distinguer deux morceaux.

### 3.1 La shortlist

La shortlist est reconstruite périodiquement à partir de l'état Kamino.

Elle sert à répondre à la question :

> parmi toutes les obligations décodées, lesquelles sont suffisamment proches de la liquidation pour mériter une surveillance active ?

Le code concerné vit surtout dans :

- [src/application/hermes_shortlist.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/application/hermes_shortlist.rs:450)

Les logs associés sont du type :

- `hermes shortlist refresh accounts`
- `hermes shortlist build`
- `hermes shortlist runtime`

### 3.2 Le stream Hermes

Une fois la shortlist produite, Jawas extrait les **feed ids Pyth** associés et ouvre un stream HTTP vers Hermes.

Le runtime écoute ensuite les changements de prix sur ces feeds :

- connexion au stream
- lecture des événements
- extraction des feeds modifiés
- matching avec les obligations shortlistées

Le code concerné est surtout ici :

- [src/application/hermes_shortlist.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/application/hermes_shortlist.rs:944)

Le log clé est :

- `hermes stream matched changed_feeds=X signals=Y`

Ce log veut dire :

> un ou plusieurs feeds shortlistés ont bougé, et cela a produit un ou plusieurs signaux candidats

---

## 4. Ce que contient une entrée Hermes

Une entrée shortlistée représente en pratique :

- une obligation Kamino
- un actif de repay surveillé
- un ou plusieurs feeds Pyth à suivre
- une distance estimée à la liquidation
- un contexte préparé pour accélérer l'exécution
- un état runtime

Ce contexte préparé est central.

Il évite de repartir entièrement à froid quand le moment de tirer arrive.

En lecture simple :

- **shortlist sans prepared context** = on sait qui surveiller
- **shortlist avec prepared context** = on sait qui surveiller et on a déjà de quoi construire plus vite le tir

---

## 5. Les états runtime à retenir

Dans les logs, Hermes manipule surtout quatre états mentaux.

### `warm`

L'obligation est shortlistée et surveillée, mais pas encore considérée comme prête à tirer.

### `armed`

L'obligation a reçu un signal prix suffisamment proche de la zone de liquidation et reste considérée comme candidate active.

C'est l'état le plus important.
Quand une obligation est `armed`, cela veut dire :

> on n'a pas seulement un candidat théorique
>
> on a un candidat actuellement dangereux

### `cooling_down`

Après un tir, l'obligation n'est pas immédiatement retirée du système.
Elle passe souvent par une phase de refroidissement pour éviter de re-émettre de manière brute.

### `dropped`

L'entrée n'est plus exploitable dans le runtime courant.

---

## 6. Comment un signal Hermes devient un tir

Le chemin logique est le suivant.

1. la shortlist est déjà en mémoire
2. le stream Hermes signale qu'un feed shortlisté a changé
3. le runtime associe ce feed à une ou plusieurs obligations
4. une entrée peut passer de `warm` à `armed`
5. le hunter reçoit un signal de type `PriceFeedPredictedLiquidable`
6. il vérifie si le firing Hermes est autorisé
7. il vérifie qu'un prepared context existe
8. il applique une mini fenêtre de confirmation
9. si tout reste valide, il appelle le chemin normal d'exécution Kamino

Le passage important dans le hunter est ici :

- [src/application/hunter.rs](/home/ppbarzin/Documents/Programmation/tools/Jawas/src/application/hunter.rs:1360)

---

## 7. Les gardes avant tir

Hermes n'envoie pas systématiquement dès qu'un feed bouge.

Il y a plusieurs gardes.

### 7.1 Le mode runtime

`HERMES_EXECUTION_MODE` détermine si Hermes :

- prépare seulement
- peut tirer en hybride
- ou devient la source principale de tir

Référence :

- [docs/specifications/hermes_hybrid_firing_v1.md](/home/ppbarzin/Documents/Programmation/tools/Jawas/docs/specifications/hermes_hybrid_firing_v1.md:1)

### 7.2 Le prepared context

Si le signal prix existe mais que le contexte n'est pas prêt, le tir est refusé.

Log associé :

- `hermes_firing_skipped`
- raison `hermes_missing_prepared_context`

Cela veut dire :

> le feed a bougé au bon moment
>
> mais le fast lane local n'avait pas encore tout préparé

### 7.3 La micro-confirmation

Le runtime peut attendre une petite fenêtre, par exemple `120 ms`, pour vérifier que le signal ne disparaît pas immédiatement.

Log associé :

- `hermes_firing_candidate`

Cette étape sert à éviter un tir trop impulsif sur un micro-mouvement fugace.

### 7.4 La fraîcheur du contexte

Même si une obligation était armée, on peut refuser le tir si le contexte préparé est déjà trop vieux.

Raisons possibles :

- `hermes_context_stale`
- `hermes_not_armed_anymore`
- `hermes_feed_match_insufficient`

---

## 8. Ce que signifient les logs les plus importants

### `hermes shortlist build`

Ce log résume la reconstruction du sous-ensemble candidat.

Exemple de lecture :

- `eligible` = obligations décodées pouvant théoriquement intéresser le runtime
- `skipped_no_wallet_repay` = obligations non exploitables car le wallet ne couvre pas l'actif de repay
- `top=...` = les candidats les plus proches

### `hermes shortlist runtime active=... states=warm=... armed=...`

C'est la photo runtime la plus utile.

Elle dit :

- combien de candidats sont actifs
- combien sont simplement `warm`
- combien sont actuellement `armed`
- combien sont en `cooling_down`

### `hermes stream matched changed_feeds=X signals=Y`

Le stream prix a bougé et a produit des signaux exploitables.

### `hermes_signal_received`

Le hunter a vu un signal d'origine Hermes et l'a corrélé à une obligation.

### `hermes_firing_candidate`

L'obligation est considérée comme assez sérieuse pour entrer dans la séquence de tir.

### `hermes_firing_skipped`

Hermes a vu quelque chose mais a volontairement refusé de tirer.

Il faut toujours lire la `reason`.

### `firing`

Le runtime a lancé le vrai chemin d'exécution Kamino.

### `bundle_sent`

Le bundle a été accepté par l'endpoint Jito.
Cela ne prouve pas une victoire on-chain.

---

## 9. Ce que Hermes améliore réellement

Hermes améliore surtout trois choses.

### 9.1 La préparation amont

On ne part plus systématiquement d'une obligation inconnue au moment du signal.

### 9.2 La réduction du champ de bataille

On ne surveille pas tout Solana en temps réel.
On surveille un petit lot déjà proche de la zone dangereuse.

### 9.3 Le fast lane

Quand les bonnes conditions sont réunies, le tir passe avec :

- `prepared_context_used=true`
- `fast_lane_used=true`

Dans les logs de la nuit du `2026-05-14`, c'est justement ce qu'on voit sur `jbHe...` et `ELHW...`.

La lecture pratique est :

> sur ces cas-là, Hermes a probablement réduit le problème "on découvre trop tard"

---

## 10. Ce que Hermes ne résout pas

Il faut être très clair ici.

Hermes ne résout pas automatiquement :

- l'inclusion on-chain
- la concurrence d'autres bots
- les rate limits Jito
- le bon calibrage du tip
- le fait qu'un bundle accepté ne land pas

Donc Hermes peut améliorer le moment où l'on voit et prépare la cible, tout en laissant entier le problème :

> "on voit assez tôt, on tire, mais on ne gagne toujours pas"

C'est exactement la transition analytique importante du projet.

---

## 11. Lecture de la nuit du 2026-05-14 à travers Hermes

Sur la fenêtre analysée cette nuit-là :

- les obligations `jbHe...` et `ELHW...` apparaissent en tête de shortlist
- elles passent en `armed`
- les tirs utilisent bien le fast lane
- les latences signal -> firing sont courtes

Donc, pour ces cas-là, la lecture n'est plus :

> "on a vu l'obligation quand la bataille était déjà finie"

La lecture devient plutôt :

> "on a vu l'obligation assez tôt pour tirer, mais on a perdu ensuite dans la couche d'exécution / d'envoi"

Autrement dit :

- **Hermes semble utile**
- **Hermes ne clôt pas la boucle**

---

## 12. La phrase de synthèse à retenir

Si tu devais garder une seule phrase :

> **Hermes dans Jawas est un système de pré-sélection, d'armement et de fast lane sur quelques obligations Kamino déjà proches de la liquidation.**

Et si tu devais en garder deux :

> **Hermes aide surtout à ne plus découvrir trop tard certaines obligations.**
>
> **Mais gagner la liquidation dépend encore de la couche d'exécution, notamment Jito.**
