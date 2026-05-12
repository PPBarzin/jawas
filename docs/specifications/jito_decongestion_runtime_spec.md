# Décongestion runtime Jito

> Statut : note technique interne
> Date : 2026-05-12
> Objet : lisser localement les appels `sendBundle` côté hunter Kamino avant d'évaluer un soak prod de 24h

## Constat

`bundle_sent` ne prouve qu'une acceptation de `sendBundle` par le block engine Jito.
Il ne prouve ni inclusion on-chain, ni victoire sur la concurrence.

Une partie des pertes reste liée au signal tardif. C'est pour cela que la préparation shortlist/Hermes est renforcée en parallèle.

Mais ce n'est pas la seule cause visible dans les traces. Une autre partie des pertes vient d'échecs Jito du type :

- `rate limit exceeded`
- `network congested`
- variantes équivalentes côté provider

Autrement dit : on perd aussi des cas encore jouables au moment de l'envoi, simplement parce que le process pousse plusieurs `sendBundle` trop près les uns des autres.

## Pourquoi agir maintenant

Ce maillon dégrade le funnel sans attendre un chantier plus lourd de `shadow state`.

Laisser le process spammer `sendBundle` pendant une rafale est moins honnête qu'une régulation locale explicite :

- le quota externe décide à notre place
- plusieurs tirs échouent de manière bruitée
- la lecture des traces mélange signal tardif et auto-congestion locale

Le but du patch n'est donc pas de "gagner plus" par magie.
Le but est d'éviter des échecs auto-infligés pour mesurer plus proprement le reste du funnel.

## Failure mode actuel

Le mode de défaillance visé est le suivant :

1. plusieurs signaux Kamino arrivent dans une fenêtre très courte
2. plusieurs tâches hunter tentent `sendBundle` dans la même seconde
3. Jito refuse une partie des appels au quota ou sous congestion
4. le résultat visible est une série de `bundle_send_failed` qui ne distinguent pas assez clairement concurrence réelle et rafale locale

## Décision technique retenue

Le patch introduit un gate process-wide sur le chemin `sendBundle` du hunter Kamino.

Règles retenues :

- un seul `sendBundle` peut être en cours à la fois
- un intervalle minimum configurable est imposé entre deux envois Jito
- un budget d'attente borné décide si un tir attend ou non

Variables runtime :

- `JITO_MIN_SEND_INTERVAL_MS=1100`
- `JITO_SEND_WAIT_BUDGET_MS=150`

Comportement :

- si le gate est libre, le tir envoie immédiatement
- si le gate est occupé mais peut se libérer dans le budget d'attente, le tir attend
- si l'attente dépasserait le budget, le tir n'envoie pas à Jito
- le retry Jito existant passe lui aussi par ce gate

Traçabilité :

- le skip dédié est journalisé avec la raison stable `jito_rate_gate_busy`
- la trace embarque aussi le détail de gate utile au diagnostic (`waited_for_lock`, `required_wait`, `wait_budget`, `min_send_interval`)

## Ce que ce patch ne fait pas

Le patch ne cherche pas à résoudre tout le problème Jito.

Il ne fait pas :

- de fallback RPC automatique
- de décision d'inclusion on-chain
- de régulation sur `getBundleStatuses`
- de pilotage avancé du tip au-delà du retry déjà présent

Le périmètre est volontairement restreint au lissage local de `sendBundle`.

## Tradeoff assumé

Le tradeoff est explicite :

- on préfère perdre proprement certains tirs concurrents avec `jito_rate_gate_busy`
- plutôt que faire échouer plusieurs appels provider dans une rafale peu interprétable

Ce choix peut réduire le volume brut de `bundle_sent` sur certaines pointes.
Il est assumé si la baisse est compensée par une chute plus nette des échecs Jito dus au quota ou à la congestion.

## Ce qu'on observera après déploiement

Pendant le soak prod de 24h, on observera surtout :

- `bundle_retry`
- `bundle_send_failed`
- `jito_rate_gate_busy`
- l'évolution brute de `bundle_sent`

Lecture attendue :

- moins d'échecs `bundle_send_failed` liés au provider Jito
- éventuellement quelques skips `jito_rate_gate_busy`, mais lisibles et assumés
- une lecture plus propre de ce qui reste vraiment imputable au signal tardif

Si `source_obligation_healthy` reste dominant malgré cette décongestion, cela renforcera l'idée que le prochain chantier principal reste la préparation amont plus lourde de type `shadow state`.
