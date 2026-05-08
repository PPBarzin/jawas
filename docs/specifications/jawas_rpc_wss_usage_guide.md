# Comment Jawas doit utiliser RPC et WSS

> Statut : note pédagogique appliquée
>  
> Date : 2026-05-05
>  
> Public visé : débutant qui comprend maintenant les mots, et veut comprendre quoi faire concrètement dans Jawas

---

## 1. Le problème réel de Jawas

La mauvaise question serait :

> "Quelle est la meilleure méthode RPC ?"

La bonne question est :

> "À quel moment Jawas a besoin de quelle information, avec quel niveau d'urgence, et à quel coût ?"

Parce que Jawas n'est pas un explorateur de blocs.

Jawas essaie de répondre à une question de recherche beaucoup plus précise :

> pourquoi un bot de liquidation réactif reste en retard, même quand il sait écouter la chaîne

Donc, le sujet RPC/WSS n'est pas seulement un sujet d'infrastructure.

C'est un sujet de :

- qualité du signal
- latence
- bruit
- coût de lecture
- charge de décodage
- discipline sur le hot path

---

## 2. Ce que Jawas fait aujourd'hui, en très simple

À la date du 2026-05-05, Jawas utilise déjà une logique assez claire :

- un provider RPC principal
- un endpoint WSS pour les signaux en temps réel
- des lectures HTTP RPC pour récupérer les détails utiles
- un mode observer
- un mode hunter
- la possibilité d'un RPC secondaire côté signal hunter

Concrètement, le code actuel ouvre un abonnement `logsSubscribe` sur le programme ciblé, puis recharge ensuite des données utiles via HTTP RPC.

Autrement dit, Jawas suit déjà l'intuition saine :

> **WSS pour détecter**
>
> **HTTP RPC pour relire et décider**

Le point important est que cette architecture est raisonnable, mais elle a une limite structurelle :

> si le signal WSS arrive déjà tard, ou si la relecture HTTP coûte trop cher, Jawas reste un bot réactif en retard

---

## 3. Les trois grandes missions de Jawas

Pour bien choisir les flux, il faut découper le problème.

Jawas n'a pas un seul besoin.

Il en a au moins trois.

### Mission 1 : Observer

Question :

> "Qu'est-ce qui vient de se passer sur la chaîne ?"

Objectif :

- capturer les liquidations visibles
- mesurer le délai
- identifier les concurrents
- constituer une matière d'analyse

Ici, on accepte un peu plus de bruit si cela améliore la couverture.

### Mission 2 : Détecter une opportunité exploitable

Question :

> "Ce signal mérite-t-il un traitement immédiat ?"

Objectif :

- filtrer le bruit
- repérer une cible potentielle
- minimiser les lectures inutiles

Ici, la vitesse commence à compter beaucoup plus.

### Mission 3 : Tirer

Question :

> "Est-ce que j'ai assez d'information pour construire et envoyer une transaction maintenant ?"

Objectif :

- éviter les relectures inutiles
- réduire le chemin critique
- agir avant les autres

Ici, chaque lecture supplémentaire peut coûter la victoire.

---

## 4. Le grand principe à retenir

Pour Jawas, il faut penser en couches :

### Couche A : Signal précoce

Un flux WSS ou gRPC dit :

> "Il se passe quelque chose d'intéressant"

### Couche B : Qualification rapide

Quelques lectures RPC disent :

> "Oui, ce cas vaut la peine d'être traité"

### Couche C : Action

Le moteur dit :

> "J'ai déjà assez préparé le terrain pour envoyer"

L'erreur classique est de vouloir tout faire dans la couche B.

Autrement dit :

- écouter
- relire 12 comptes
- recalculer toute la terre
- découvrir seulement à la fin que la cible n'est pas bonne

Cette manière de faire est correcte pour un outil d'analyse.

Elle est souvent trop lente pour une course réelle.

---

## 5. À quoi sert `logsSubscribe` dans Jawas

`logsSubscribe` est très adapté à une première génération de Jawas.

Pourquoi ?

Parce que cette méthode répond bien à la question :

> "Est-ce qu'une transaction impliquant ce programme a laissé une trace intéressante ?"

### Ce que `logsSubscribe` fait bien

- capter rapidement une activité autour du programme
- voir qu'une instruction de liquidation a probablement été tentée
- déclencher un pipeline d'analyse
- rester assez simple à implémenter

### Ce que `logsSubscribe` fait moins bien

- les logs sont parfois plus textuels que structurés
- il faut souvent recharger la transaction ou des comptes pour comprendre proprement
- le flux peut contenir beaucoup d'événements sans valeur pour l'action

### Quand `logsSubscribe` est un bon choix pour Jawas

- pour l'observer
- pour mesurer le timing d'un concurrent
- pour construire une base de cas
- pour un hunter encore majoritairement réactif

### Quand `logsSubscribe` montre sa limite

- quand le coût de relecture derrière est trop élevé
- quand le signal visible arrive après les meilleurs concurrents
- quand la simple détection d'une liquidation déjà partie ne donne plus d'avantage actionnable

En résumé :

> `logsSubscribe` est excellent pour apprendre ce qui se passe
>
> il n'est pas automatiquement suffisant pour gagner la course

---

## 6. À quoi sert `programSubscribe` dans Jawas

`programSubscribe` sert à écouter les changements de comptes appartenant à un programme.

La promesse naïve serait :

> "Si j'écoute tout Kamino, je verrai tout"

Oui, mais ce n'est pas gratuit.

### Ce que `programSubscribe` peut apporter

- voir les changements d'état de comptes du protocole
- suivre les obligations, réserves ou autres comptes gérés par le programme
- potentiellement détecter un changement avant de le reconstruire autrement

### Son vrai problème

Il peut devenir extrêmement bavard.

Si le programme possède beaucoup de comptes actifs, Jawas risque de recevoir :

- trop de messages
- trop de décodage
- trop de bruit
- trop de charge CPU pour trier ce qui compte vraiment

### Quand `programSubscribe` vaut la peine

- si le périmètre est petit
- si tu filtres très bien en amont
- si tu surveilles un sous-ensemble de comptes déjà présélectionnés

### Quand il est probablement trop cher

- si tu veux écouter un programme très large sans préparation
- si ton bot doit ensuite relire presque tout de toute façon

Pour Jawas, la leçon n'est pas :

> "`programSubscribe` est mauvais"

La leçon est :

> "`programSubscribe` est dangereux si tu n'as pas déjà une stratégie de réduction du bruit"

---

## 7. À quoi sert `accountSubscribe` dans Jawas

`accountSubscribe` est beaucoup plus chirurgical.

Tu surveilles un compte précis.

Exemple mental :

- une obligation très proche de la liquidation
- un compte oracle important
- un compte de réserve critique

### Son intérêt réel

Quand Jawas a déjà préparé une watchlist, `accountSubscribe` devient très fort.

Parce qu'au lieu de demander :

> "Que se passe-t-il dans tout Kamino ?"

tu demandes :

> "Préviens-moi si CETTE cible quasi mûre bouge."

### Quand `accountSubscribe` devient meilleur que `logsSubscribe`

Quand Jawas cesse d'être purement réactif et devient plus préparé.

Autrement dit :

- tu as déjà identifié des obligations proches du seuil
- tu sais quels comptes comptent
- tu veux réduire les lectures générales

Là, `accountSubscribe` peut devenir plus intéressant qu'un flux global plus bavard.

---

## 8. À quoi sert `transactionSubscribe` dans Jawas

`transactionSubscribe` est utile quand tu veux un flux transactionnel plus structuré que de simples logs.

La bonne intuition est :

> c'est un outil de filtrage de transactions plus directement exploitable

### Ce que cela peut améliorer

- réduire la part d'analyse textuelle des logs
- cibler certaines transactions ou certains comptes mentionnés
- récupérer plus vite un contexte transactionnel exploitable

### Ce que cela ne règle pas magiquement

- la concurrence réseau
- la nécessité éventuelle de relire des comptes
- le coût de décision local
- le fait que le bot soit déjà trop tard dans la chaîne de causalité

Pour Jawas, `transactionSubscribe` est potentiellement utile si l'objectif est :

- mieux qualifier le signal
- réduire la plomberie d'après-log
- comparer la qualité du signal face à `logsSubscribe`

Mais la question à poser n'est pas :

> "Est-ce plus moderne ?"

La question correcte est :

> "Est-ce que cela réduit vraiment le temps utile entre détection et décision ?"

---

## 9. Ce qu'il ne faut pas faire sur le hot path

Le hot path, c'est le chemin :

> signal reçu -> décision -> envoi

Sur ce chemin, Jawas doit devenir brutalement pragmatique.

Il faut éviter autant que possible :

- recharger trop de comptes
- recalculer des choses déjà prévisibles
- parser des masses de données peu utiles
- dépendre d'un enrichissement provider si ce n'est pas nécessaire
- attendre une confirmation plus forte que le besoin réel

La règle d'école est simple :

> sur le hot path, chaque lecture doit justifier son existence

Si une lecture ne change presque jamais la décision finale, elle n'a probablement pas sa place dans le chemin critique.

---

## 10. Ce que Jawas doit précomputER hors hot path

Le vrai gain n'est pas seulement de choisir un meilleur flux.

Le vrai gain est souvent de déplacer du travail en dehors du moment critique.

Jawas devrait tendre vers une architecture où il précompute :

- les obligations déjà proches du seuil
- les comptes nécessaires à une liquidation
- les mints et marchés vraiment supportés
- les contraintes liées au wallet
- les paramètres de décision déjà connus

Ainsi, quand un signal arrive, Jawas ne découvre pas le monde.

Il vérifie juste :

> "Cette cible préparée est-elle maintenant assez mûre pour tirer ?"

Cette transition est probablement plus importante qu'un simple changement de méthode WSS.

---

## 11. Observer et Hunter n'ont pas les mêmes besoins

C'est un point crucial.

Si tu choisis la même stratégie de flux pour les deux, tu risques de mal servir les deux.

### Observer

L'observer veut :

- beaucoup voir
- bien documenter
- bien mesurer
- accepter plus de bruit

Un `logsSubscribe` assez large peut être très défendable ici.

### Hunter

Le hunter veut :

- peu voir mais très tôt
- réduire le coût de décision
- ne pas relire le monde entier
- transformer un signal en action en un minimum d'étapes

Le hunter a donc intérêt à devenir plus sélectif et plus préparé.

Conclusion :

> le bon flux pour observer n'est pas forcément le bon flux pour chasser

---

## 12. Quand un provider standard suffit

Un provider standard RPC + WSS suffit encore si l'objectif principal est :

- observer des liquidations
- rejouer des cas
- comparer les délais
- documenter les concurrents
- tester des idées d'architecture

Dans ce cadre, la priorité n'est pas d'avoir l'infrastructure ultime.

La priorité est de :

- mesurer proprement
- tracer correctement
- réduire les lectures inutiles

Autrement dit :

> tant que Jawas apprend encore où il perd, un provider standard bien utilisé peut suffire

---

## 13. Quand un flux plus avancé devient pertinent

Un flux type `transactionSubscribe` avancé, `LaserStream`, `Yellowstone gRPC` ou équivalent devient pertinent quand le diagnostic est plus clair.

Par exemple :

- on sait que le signal actuel arrive trop tard
- on sait que le problème principal n'est plus le décodage local
- on sait qu'une meilleure fraîcheur des événements changerait réellement la décision
- on a déjà réduit les relectures inutiles

Sinon, il y a un risque classique :

> acheter une meilleure tuyauterie alors que le goulet principal est encore dans la pièce d'après

Très souvent, le premier vrai gain vient de la discipline logicielle avant de venir d'une infra premium.

---

## 14. Comment décider entre `logsSubscribe` et `transactionSubscribe`

Voici une règle simple pour Jawas.

### Préfère `logsSubscribe` si :

- tu veux un pipeline simple
- tu es surtout en mode observation
- tu veux détecter qu'une instruction de liquidation a eu lieu
- tu acceptes de recharger ensuite pour comprendre

### Préfère tester `transactionSubscribe` si :

- tu veux comparer la qualité de signal
- tu veux un flux transactionnel plus directement exploitable
- tu soupçonnes que le parsing de logs te fait perdre du temps ou de la précision

La bonne attitude n'est pas de trancher par intuition.

La bonne attitude est :

> mesurer les deux sur les mêmes cas et comparer leur délai utile

Pas seulement :

- délai de réception du message

Mais :

- délai jusqu'à une décision exploitable

---

## 15. Comment décider entre `logsSubscribe` et `accountSubscribe`

La règle est encore plus simple.

### Si Jawas ne sait pas encore quelles cibles suivre

`logsSubscribe` est souvent plus naturel.

### Si Jawas a déjà une watchlist qualifiée

`accountSubscribe` devient beaucoup plus intéressant.

Parce que le bot passe d'une posture :

> "je regarde passer le trafic"

à une posture :

> "je surveille mes cibles déjà préparées"

Cette bascule correspond exactement au passage :

- de la réaction
- vers la préparation

Et c'est le cœur de la question de recherche du projet.

---

## 16. Où sont probablement les vrais goulets d'étranglement

Il faut rester honnête.

Dans un bot de liquidation Solana, perdre ne vient pas toujours du provider.

Les goulets plausibles sont :

### 1. Le signal lui-même

Le bot apprend l'événement trop tard.

### 2. La relecture RPC

Le bot relit trop d'état dans l'urgence.

### 3. Le décodage local

Le bot sait recevoir le message, mais met trop de temps à reconstruire le contexte utile.

### 4. La décision

Le bot hésite trop longtemps parce qu'il vérifie trop de conditions à chaud.

### 5. L'envoi

Le bot construit ou route la transaction trop lentement.

### 6. La stratégie elle-même

Le bot poursuit des cibles déjà condamnées par la concurrence.

Autrement dit :

> changer de provider sans localiser précisément la perte peut produire très peu de valeur

---

## 17. Une stratégie raisonnable pour Jawas

Si l'on reste discipliné, la trajectoire logique ressemble à ceci.

### Étape 1

Garder `logsSubscribe` comme base d'observation robuste.

Objectif :

- continuer à mesurer
- continuer à constituer des cas
- comparer les sources

### Étape 2

Réduire les lectures HTTP sur le hot path.

Objectif :

- identifier les lectures réellement décisives
- sortir le reste hors du moment critique

### Étape 3

Construire une watchlist préarmée.

Objectif :

- passer d'un bot qui découvre à un bot qui attend des bascules précises

### Étape 4

Tester un flux plus ciblé :

- `accountSubscribe` sur des cibles préparées
- ou `transactionSubscribe` si le flux transactionnel est plus directement exploitable

### Étape 5

Seulement ensuite, réévaluer le besoin d'une infra plus spécialisée :

- provider secondaire de signal
- LaserStream
- Yellowstone gRPC
- autre source très faible latence

Cette séquence évite de confondre :

- problème de méthode
- problème d'architecture
- problème de stratégie

---

## 18. Ce que je recommanderais pédagogiquement pour Jawas

Si je devais expliquer cela à un bleu sans vendre du rêve, je dirais :

### Pour l'observer

Garde une approche large, lisible, stable.

`logsSubscribe` est un très bon professeur.

Il montre la vie réelle de la chaîne, même s'il ne te fait pas gagner la course.

### Pour le hunter

Ne cherche pas d'abord "le provider miracle".

Cherche d'abord :

- quelles lectures sont évitables
- quelles cibles peuvent être précomputées
- quelles opportunités sont réellement jouables avec le wallet et le temps disponible

### Pour la suite

Si Jawas prouve que son problème principal est bien la fraîcheur du signal, alors oui, une couche plus avancée de streaming mérite d'être testée sérieusement.

Mais pas avant d'avoir démontré que :

- la logique locale est déjà disciplinée
- le chemin critique est déjà court
- la stratégie n'est pas fondamentalement trop réactive

---

## 19. Résumé ultra simple

Si tu ne devais retenir que cela :

1. `logsSubscribe` est très bon pour observer et apprendre.
2. `programSubscribe` peut être puissant, mais devient vite trop bruyant.
3. `accountSubscribe` devient très intéressant quand Jawas a déjà une watchlist préparée.
4. `transactionSubscribe` vaut la peine si son signal réduit vraiment le temps utile jusqu'à la décision.
5. Le vrai gain vient souvent moins du provider que de la réduction des lectures et du précomputing hors hot path.
6. La transition importante pour Jawas est le passage de la réaction vers la préparation.

---

## 20. Suite logique

La suite la plus utile serait probablement une troisième note, encore plus concrète, par exemple :

`cartographie du hot path Jawas`

Elle pourrait lister, étape par étape :

- ce qui arrive aujourd'hui après un signal
- quelles lectures HTTP sont faites
- lesquelles sont indispensables
- lesquelles peuvent être précomputées
- où se perdent réellement les millisecondes et les secondes
