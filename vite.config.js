import { resolve } from 'path';
import { cpSync, existsSync } from 'node:fs';
import { defineConfig } from 'vite';

export default defineConfig({
    build: {
        rollupOptions: {
            input: {
                script: resolve(__dirname, 'crates/arc-app/src/resources/js/script.js'),
                styles: resolve(__dirname, 'crates/arc-app/src/resources/css/styles.css'),
            },
            output: {
                entryFileNames: '[name]-[hash].js',
                chunkFileNames: '[name]-[hash].js',
                assetFileNames: '[name]-[hash][extname]',
            },
        },
        outDir: 'dist',
        emptyOutDir: true,
        cssCodeSplit: true,
        manifest: true,
    },
    plugins: [
        {
            name: 'arc-copy-images',
            writeBundle() {
                const source = resolve(__dirname, 'crates/arc-app/src/resources/imgs');
                if (existsSync(source)) cpSync(source, resolve(__dirname, 'dist/imgs'), { recursive: true });
            },
        },
    ],
});
