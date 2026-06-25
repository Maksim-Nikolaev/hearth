SHELL := /bin/bash
-include .env
export

AGE_KEY_FILE ?= $(or $(SOPS_AGE_KEY_FILE),$(HOME)/.config/age/keys.txt)

.PHONY: up down rebuild update logs ps psql seed secrets-encrypt secrets-decrypt

up: ## Start the full stack (detached)
	docker compose up -d

down: ## Stop the stack
	docker compose down

rebuild: ## Rebuild the backend image
	docker compose build backend

update: ## Rebuild + restart the backend with new code
	docker compose up -d --build backend

logs: ## Follow logs
	docker compose logs -f

ps: ## Show stack status
	docker compose ps

psql: ## Open a psql shell in the Postgres container
	docker compose exec postgres psql -U $(POSTGRES_USER) -d $(POSTGRES_DB)

seed: ## Seed dev users (alice/bob) in the running stack's DB
	docker compose run --rm backend seed

secrets-encrypt: ## Encrypt .env -> .env.enc (sops + age, recipient from .sops.yaml)
	@command -v sops >/dev/null || { echo "install sops: https://github.com/getsops/sops"; exit 1; }
	sops --encrypt --input-type dotenv --output-type dotenv .env > .env.enc
	@echo "encrypted .env -> .env.enc"

secrets-decrypt: ## Decrypt .env.enc -> .env (needs the age key in AGE_KEY_FILE)
	@command -v sops >/dev/null || { echo "install sops: https://github.com/getsops/sops"; exit 1; }
	SOPS_AGE_KEY_FILE="$(AGE_KEY_FILE)" sops --decrypt --input-type dotenv --output-type dotenv .env.enc > .env
	@echo "decrypted .env.enc -> .env"
