rm hunter*
docker cp jawas-kamino:/app/hunter_signal_metrics.jsonl . 
docker cp jawas-kamino:/app/hunter_trace.jsonl . 
scp hunter* ppbarzin@Innov8lab:~/Documents/Programmation/tools/Jawas
