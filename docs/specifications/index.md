# Spécifications

Cette section contient les spécifications de design et les contrats de comportement de Jawas.

## Éducatif — Comprendre les briques

| Document | Description |
|----------|-------------|
| [RPC & WebSocket](rpc_wss_helius_quicknode_explained.md) | Concepts RPC, providers Helius/QuickNode, différences HTTP vs WebSocket |
| [Jito — Bundles](jito_explained_for_jawas.md) | Infrastructure de soumission de bundles Jito et rôle dans la liquidation |
| [Hermes — Price feeds](hermes_explained_for_jawas.md) | Rôle de Hermes : pré-armement shortlist + exécution rapide déclenchée par prix |
| [Usage RPC dans Jawas](jawas_rpc_wss_usage_guide.md) | Guide d'usage basé sur les rôles : signal timing, points de décision |

## Runtime & Comportement

| Document | Description |
|----------|-------------|
| [Bot de liquidation](liquidation_bot_spec.md) | Concept du bot, mécanique, exemples |
| [Runtime de production](jawas_production_runtime.md) | Comment Jawas fonctionne réellement (observer + hunter) |
| [Phase 2 — Workflow](phase2_liquidation_workflow.md) | Workflow du hunter avec diagramme Mermaid |
| [Hermes — State machine de tir](hermes_firing_state_machine.md) | Flowchart et transitions `Warm`/`Armed`/`CoolingDown`/`Dropped` jusqu'au bundle Jito |

## Features

| Document | Description |
|----------|-------------|
| [P1 — Shortlist proactive](p1_proactive_shortlist_hybrid_spec.md) | Spec P1 : filtrage wallet-first, max 10 obligations |
| [Hermes v1 — Firing](hermes_hybrid_firing_v1.md) | Contrat de firing Hermes v1, modes runtime, politique de confirmation |
| [Jito — Décongestion](jito_decongestion_runtime_spec.md) | Rate limiting bundle send, prévention congestion auto-infligée |

## Analyse expert

| Document | Description |
|----------|-------------|
| [Analyse de retard](analyse-de-retard-expert1.md) | Pourquoi Jawas reçoit les signaux trop tard — gRPC vs WebSocket latency |
