# Engineering Handbook

Welcome to the engineering team! This handbook covers everything you need to
get started as a new hire.

## Development Environment Setup

### Prerequisites

Before you begin, make sure you have the following installed:

- **Git** (v2.40 or later)
- **Node.js** (v20 LTS) via nvm
- **Docker Desktop** (or Podman)
- **VS Code** with recommended extensions

### Getting Started

1. Clone the repo:
   ```bash
   git clone git@github.com:example/monorepo.git
   cd monorepo
   ```
2. Install dependencies:
   ```bash
   npm install
   ```
3. Copy the example environment file:
   ```bash
   cp .env.example .env.local
   ```
4. Start the development server:
   ```bash
   npm run dev
   ```
5. Open http://localhost:3000 in your browser.

### VPN Access

Request VPN access through the IT portal at https://it.internal/vpn-request.
You will need your manager's approval. Typical turnaround is 1 business day.

## Code Review Process

All changes go through pull requests on GitHub.

1. Create a feature branch from `main`.
2. Make your changes and push to origin.
3. Open a pull request with a clear description.
4. Request at least two reviewers from your team.
5. Address all feedback before merging.
6. Squash-merge into `main` once approved.

Reviews should be completed within 24 hours. If a review is blocking you,
reach out in the #engineering Slack channel.

## Deployment Process

### Staging

1. Merge your PR to `main`.
2. The CI pipeline runs automatically (lint, test, build).
3. If CI passes, the staging deployment triggers automatically.
4. Verify your changes at https://staging.example.com.
5. Run smoke tests: `npm run test:smoke -- --env staging`

### Production

1. Create a release PR from `main` to `production`.
2. Get sign-off from the on-call engineer.
3. Merge the release PR.
4. Monitor the deploy dashboard for 15 minutes.
5. If issues arise, roll back via the deploy dashboard.

## On-Call Rotation

The on-call rotation covers all production services. Each rotation lasts one
week, Monday to Monday.

### Current On-Call Schedule

| Team      | Primary      | Secondary    |
|-----------|-------------|-------------|
| Platform  | Bob Smith    | Carol Davis  |
| Payments  | Alice Chen   | Dan Wilson   |
| Frontend  | Eve Park     | Frank Lopez  |

### Responsibilities

- Respond to pages within 15 minutes.
- Triage incoming alerts and escalate if needed.
- Post incident summaries in #incidents after resolution.
- Update runbooks if you discover undocumented procedures.

## Architecture Overview

The monorepo contains the following services:

- **api-gateway** - Express.js, routes requests to backend services.
- **user-service** - Handles authentication and user profiles (PostgreSQL).
- **payments-service** - Billing and subscriptions (PostgreSQL, Stripe).
- **notification-service** - Email and push notifications (Redis, SQS).
- **frontend** - Next.js web application.

All services communicate via REST APIs internally. The API gateway handles
external traffic and rate limiting.

## Database

The payments service uses PostgreSQL 15 with read replicas for reporting
queries. Connection pooling is handled by PgBouncer.

### Required Parameters for POST /api/v2/users

The following fields are required (mandatory) when creating a new user:

- `email` (string) - The user's email address. Must be unique.
- `name` (string) - The user's full name.

Optional fields include `role`, `team`, and `avatar_url`.

## SLA

Our API uptime SLA is 99.9%, measured monthly. This allows approximately
43 minutes of downtime per month. Uptime is tracked via our status page
at https://status.example.com.
