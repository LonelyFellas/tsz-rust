```bash
docker compose up -d db        # 后台起 Postgres
docker compose ps              # 看状态（等 healthy）
docker compose logs -f db      # 看日志
docker compose down            # 停（数据留在卷里）
docker compose down -v         # 停并删数据（重置）
```