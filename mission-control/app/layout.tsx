import type { Metadata } from 'next';
import './globals.css';

export const metadata: Metadata = {
    title: 'SpaceOrb V7.6 — Mission Control',
    description: 'Ground Station Dashboard for the SpaceOrb Autonomous Orbital Edge Daemon',
};

export default function RootLayout({
    children,
}: {
    children: React.ReactNode;
}) {
    return (
        <html lang="en">
            <body className="antialiased">
                {children}
            </body>
        </html>
    );
}
