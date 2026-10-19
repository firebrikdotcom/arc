.PHONY: help install build dev serve migrate seed test clean format check lint doctor arc-check publish-check scaffold-check benthos-config benthos-config-check benthos-lint docs-check docker-build docker-up docker-down e2e e2e-install e2e-build e2e-headed e2e-report docsify docs

# Default target
.DEFAULT_GOAL := help

# Colors for output
CYAN := \033[0;36m
GREEN := \033[0;32m
YELLOW := \033[0;33m
NC := \033[0m # No Color

##@ General

help: ## Display this help message
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage:\n  make $(CYAN)<target>$(NC)\n"} /^[a-zA-Z_-]+:.*?##/ { printf "  $(CYAN)%-15s$(NC) %s\n", $$1, $$2 } /^##@/ { printf "\n$(YELLOW)%s$(NC)\n", substr($$0, 5) } ' $(MAKEFILE_LIST)

##@ Setup & Installation

# Install all project dependencies including Rust crates, npm packages, and cargo-watch
install: ## Install all dependencies (Rust, npm, and cargo-watch)
	@echo "$(GREEN)Installing Rust dependencies...$(NC)"
	cargo fetch
	@echo "$(GREEN)Installing npm dependencies...$(NC)"
	npm install
	@echo "$(GREEN)Updating browserslist database...$(NC)"
	npx update-browserslist-db@latest --yes
	@echo "$(GREEN)Checking for cargo-watch...$(NC)"
	@if ! command -v cargo-watch >/dev/null 2>&1; then \
		echo "$(YELLOW)Installing cargo-watch...$(NC)"; \
		cargo install cargo-watch; \
	else \
		echo "$(GREEN)cargo-watch already installed$(NC)"; \
	fi
	@echo "$(GREEN)Dependencies installed successfully!$(NC)"

# Verify required system packages: build-essential, libssl-dev, libsqlite3-dev
deps-check: ## Check if all required system dependencies are installed
	@echo "$(GREEN)Checking system dependencies...$(NC)"
	@command -v gcc >/dev/null 2>&1 || (echo "$(YELLOW)Warning: build-essential not found$(NC)" && exit 1)
	@pkg-config --exists openssl 2>/dev/null || (echo "$(YELLOW)Warning: libssl-dev not found$(NC)" && exit 1)
	@pkg-config --exists sqlite3 2>/dev/null || (echo "$(YELLOW)Warning: libsqlite3-dev not found$(NC)" && exit 1)
	@echo "$(GREEN)All system dependencies are installed$(NC)"

##@ Development

# Run dev server with hot-reload, watches Rust code changes and bundles frontend with Vite
dev: ## Start development server with hot-reload and Vite bundling
	@echo "$(GREEN)Starting development server...$(NC)"
	cargo run -p arc -- develop

# Start production server without file watching or hot-reload
serve: ## Start production server without hot-reload
	@echo "$(GREEN)Starting production server...$(NC)"
	cargo run -p arc -- serve

# Compile optimized binary with full optimizations (slower build, faster runtime)
build: ## Build the project in release mode
	@echo "$(GREEN)Building project in release mode...$(NC)"
	cargo build --release

# Compile unoptimized binary for faster compilation during development
build-dev: ## Build the project in debug mode
	@echo "$(GREEN)Building project in debug mode...$(NC)"
	cargo build

# Continuously check code and run tests on file changes using cargo-watch
watch: ## Watch for changes and rebuild (requires cargo-watch)
	@echo "$(GREEN)Watching for changes...$(NC)"
	cargo watch -x check -x test

##@ Database

# Apply all pending Diesel migrations to update database schema
migrate: ## Run database migrations
	@echo "$(GREEN)Running database migrations...$(NC)"
	cargo run -p arc -- migrate

# Create new aggregate from template
new-aggregate: ## Scaffold a new CommandBus aggregate (usage: make new-aggregate NAME=Task)
	@if [ -z "$(NAME)" ]; then \
		echo "$(YELLOW)Error: Please provide NAME parameter$(NC)"; \
		echo "Usage: make new-aggregate NAME=Task"; \
		exit 1; \
	fi
	bash scripts/new-aggregate.sh $(NAME)

# Populate database with initial sample data for development
seed: ## Seed the database with sample data
	@echo "$(GREEN)Seeding database...$(NC)"
	cargo run -p arc -- seed

# Run migrations then seed - complete database initialization
db-setup: migrate seed ## Setup database (migrate + seed)
	@echo "$(GREEN)Database setup complete!$(NC)"

# Generate new Diesel migration file. Usage: make migrate-new NAME=create_users
migrate-new: ## Create a new Diesel migration (usage: make migrate-new NAME=migration_name)
	@if [ -z "$(NAME)" ]; then \
		echo "$(YELLOW)Error: Please provide NAME parameter$(NC)"; \
		echo "Usage: make migrate-new NAME=create_users"; \
		exit 1; \
	fi
	diesel migration generate $(NAME)

##@ Testing & Quality

# Execute all unit and integration tests
test: ## Run all tests
	@echo "$(GREEN)Running tests...$(NC)"
	cargo test --workspace --all-features

# Run tests with full output including println! statements
test-verbose: ## Run tests with verbose output
	@echo "$(GREEN)Running tests (verbose)...$(NC)"
	cargo test --workspace --all-features -- --nocapture

# Generate HTML coverage report using cargo-tarpaulin (auto-installs if needed)
coverage: ## Generate test coverage report (requires cargo-tarpaulin)
	@echo "$(GREEN)Generating coverage report...$(NC)"
	@if ! command -v cargo-tarpaulin >/dev/null 2>&1; then \
		echo "$(YELLOW)Installing cargo-tarpaulin...$(NC)"; \
		cargo install cargo-tarpaulin; \
	fi
	cargo tarpaulin --out Html --output-dir coverage

# Verify code compiles without producing binary (fast validation)
check: ## Check code without building
	@echo "$(GREEN)Checking code...$(NC)"
	cargo check

# Auto-format all Rust code according to rustfmt style guidelines
format: ## Format code using rustfmt
	@echo "$(GREEN)Formatting code...$(NC)"
	cargo fmt

# Check if code is formatted correctly without modifying files
format-check: ## Check code formatting without making changes
	@echo "$(GREEN)Checking code format...$(NC)"
	cargo fmt -- --check

# Run Clippy linter with warnings treated as errors for strict quality checks
lint: ## Run clippy linter
	@echo "$(GREEN)Running clippy...$(NC)"
	cargo clippy --workspace --all-targets --all-features -- -D warnings

doctor: ## Check Arc upgradeability drift guardrails
	@echo "$(GREEN)Running Arc doctor checks...$(NC)"
	bash scripts/arc-check.sh

arc-check: doctor ## Alias for doctor, matching ADR 0002 language

publish-check: ## Package reusable crates and compile them outside the workspace
	@echo "$(GREEN)Checking publishable crate packages...$(NC)"
	bash scripts/check-publish-packages.sh $(PUBLISH_CRATES)

scaffold-check: ## Generate and compile clean-room minimal and UI Arc applications
	@echo "$(GREEN)Checking arc new outside the workspace...$(NC)"
	bash scripts/check-arc-scaffold.sh

benthos-config: ## Generate Benthos pipeline config from handler manifests
	@echo "$(GREEN)Generating Benthos config...$(NC)"
	npm run benthos:config

benthos-config-check: ## Check generated Benthos config is up to date
	@echo "$(GREEN)Checking generated Benthos config...$(NC)"
	npm run benthos:config:check

benthos-lint: benthos-config-check ## Lint Benthos pipeline config
	@echo "$(GREEN)Linting Benthos config...$(NC)"
	@if command -v redpanda-connect >/dev/null 2>&1; then \
		READ_MODEL_DATABASE_URL="$${READ_MODEL_DATABASE_URL:-unused}" redpanda-connect lint config/benthos/events.yaml config/benthos/generated/events.yaml; \
	elif command -v benthos >/dev/null 2>&1; then \
		READ_MODEL_DATABASE_URL="$${READ_MODEL_DATABASE_URL:-unused}" benthos lint config/benthos/events.yaml config/benthos/generated/events.yaml; \
	elif command -v docker >/dev/null 2>&1; then \
		docker run --rm -e READ_MODEL_DATABASE_URL=unused -v "$$(pwd):/work" -w /work docker.redpanda.com/redpandadata/connect:latest lint config/benthos/events.yaml config/benthos/generated/events.yaml; \
	else \
		echo "$(YELLOW)Install redpanda-connect, benthos, or Docker to lint Benthos configs$(NC)"; \
		exit 1; \
	fi

docs-check: ## Verify canonical documentation matches the current release and routing implementation

	@echo "$(GREEN)Checking documentation freshness...$(NC)"
	bash scripts/check-documentation-freshness.sh

# Scan dependencies for known security vulnerabilities (auto-installs cargo-audit)
audit: ## Audit dependencies for security vulnerabilities
	@echo "$(GREEN)Auditing dependencies...$(NC)"
	@if ! command -v cargo-audit >/dev/null 2>&1; then \
		echo "$(YELLOW)Installing cargo-audit...$(NC)"; \
		cargo install cargo-audit; \
	fi
	cargo audit

##@ End-to-End Tests

# One-time setup: install Playwright and the Chromium browser
e2e-install: ## Install Playwright + Chromium (one-time)
	@echo "$(GREEN)Installing Playwright...$(NC)"
	npm install -D @playwright/test
	npx playwright install --with-deps chromium

# Compile the Rust binary and build frontend assets so the E2E run hits a real, complete server
e2e-build: ## Build binary + frontend assets for E2E
	@echo "$(GREEN)Building binary + frontend for E2E...$(NC)"
	cargo build -p arc --bin arc
	npm run build

# Run the full Playwright suite. Spawns server, runs migrations + seed, executes specs, tears down.
e2e: e2e-build ## Run the Playwright E2E suite
	@echo "$(GREEN)Running E2E tests...$(NC)"
	E2E_SKIP_BUILD=1 E2E_SKIP_FRONTEND=1 npx playwright test

# Run with a visible browser for debugging
e2e-headed: e2e-build ## Run E2E tests with a visible browser
	E2E_SKIP_BUILD=1 E2E_SKIP_FRONTEND=1 npx playwright test --headed

# Open the last HTML report
e2e-report: ## Open the latest Playwright HTML report
	npx playwright show-report

##@ Frontend

# Bundle and minify CSS/JS assets for production using Vite
frontend-build: ## Build frontend assets with Vite
	@echo "$(GREEN)Building frontend assets...$(NC)"
	npm run build

# Start Vite dev server with hot module replacement for frontend development
frontend-dev: ## Run Vite development server
	@echo "$(GREEN)Starting Vite development server...$(NC)"
	npm run dev

# Install only npm/JavaScript dependencies (Vite, Hotwire, Toastify)
frontend-install: ## Install npm dependencies only
	@echo "$(GREEN)Installing npm dependencies...$(NC)"
	npm install

##@ Documentation

# Serve the docs/ site locally (http://localhost:3001 by default)
DOCSIFY_PORT ?= 3001
docsify docs: ## Serve docs/ locally with Docsify (DOCSIFY_PORT=3001)
	@echo "$(GREEN)Serving docs/ at http://localhost:$(DOCSIFY_PORT)$(NC)"
	npm run docs:serve -- --port $(DOCSIFY_PORT)

##@ Cleanup

# Remove all compiled artifacts, node_modules, and build outputs
clean: ## Remove build artifacts and dependencies
	@echo "$(GREEN)Cleaning build artifacts...$(NC)"
	cargo clean
	rm -rf node_modules
	rm -rf dist
	rm -rf target

# Delete SQLite database files with confirmation prompt (DESTRUCTIVE)
clean-db: ## Remove database file (WARNING: destructive)
	@echo "$(YELLOW)Warning: This will delete your database!$(NC)"
	@read -p "Are you sure? [y/N] " -n 1 -r; \
	echo; \
	if [[ $$REPLY =~ ^[Yy]$$ ]]; then \
		rm -f *.db *.db-shm *.db-wal; \
		echo "$(GREEN)Database files removed$(NC)"; \
	fi

##@ Utilities

# Execute arbitrary cargo commands. Usage: make run CMD="serve" or make run CMD="custom-command"
run: ## Run custom cargo command (usage: make run CMD="your command")
	@if [ -z "$(CMD)" ]; then \
		echo "$(YELLOW)Usage: make run CMD=\"your command\"$(NC)"; \
		exit 1; \
	fi
	cargo run -p arc -- $(CMD)

# Tail application log files if logs directory exists
logs: ## View application logs (if implemented)
	@echo "$(GREEN)Viewing logs...$(NC)"
	@if [ -d "logs" ]; then \
		tail -f logs/*.log; \
	else \
		echo "$(YELLOW)No logs directory found$(NC)"; \
	fi

# One-command setup for new developers: checks deps, installs packages, initializes database
setup: deps-check install db-setup ## Complete project setup for new developers
	@echo "$(GREEN)============================================$(NC)"
	@echo "$(GREEN)Setup complete! You can now run:$(NC)"
	@echo "  $(CYAN)make dev$(NC)     - Start development server"
	@echo "  $(CYAN)make test$(NC)    - Run tests"
	@echo "$(GREEN)============================================$(NC)"

##@ Environment

# Create .env from .env.example template if not exists
env-copy: ## Copy .env.example to .env
	@if [ ! -f .env ]; then \
		echo "$(GREEN)Creating .env file from .env.example...$(NC)"; \
		cp .env.example .env; \
		echo "$(YELLOW)Remember to update .env with your settings!$(NC)"; \
	else \
		echo "$(YELLOW).env file already exists$(NC)"; \
	fi

# Copy test environment configuration for running tests
env-test: ## Set up test environment
	@echo "$(GREEN)Setting up test environment...$(NC)"
	@if [ -f .env.test ]; then \
		cp .env.test .env; \
		echo "$(GREEN)Test environment configured$(NC)"; \
	else \
		echo "$(YELLOW).env.test not found$(NC)"; \
	fi
