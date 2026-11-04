FROM node:24-bookworm-slim AS build
WORKDIR /app
COPY price-service/package*.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
COPY price-service/service.ts price-service/tsconfig.json ./
RUN npm run build && npm prune --omit=dev --ignore-scripts

FROM node:24-bookworm-slim
WORKDIR /app
COPY --from=build /app/package*.json ./
COPY --from=build /app/node_modules/ ./node_modules/
COPY --from=build /app/dist/ ./dist/
ENV NODE_ENV=production BIND_HOST=0.0.0.0
USER node
CMD ["node", "dist/service.js"]
