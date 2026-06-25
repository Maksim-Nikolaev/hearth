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

secrets-encrypt: ## Encrypt .env -> .env.age (needs AGE_RECIPIENTS)
	@command -v age >/dev/null || { echo "install age: https://github.com/FiloSottile/age"; exit 1; }
	@test -n "$(AGE_RECIPIENTS)" || { echo "set AGE_RECIPIENTS (path to a recipients file, one age1... per line)"; exit 1; }
	age -R "$(AGE_RECIPIENTS)" -o .env.age .env
	@echo "encrypted .env -> .env.age"

secrets-decrypt: ## Decrypt .env.age -> .env (needs AGE_KEY_FILE)
	@command -v age >/dev/null || { echo "install age: https://github.com/FiloSottile/age"; exit 1; }
	age -d -i "$(AGE_KEY_FILE)" -o .env .env.age
	@echo "decrypted .env.age -> .env"
