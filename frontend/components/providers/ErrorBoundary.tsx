"use client";

import React, { Component, ReactNode } from "react";

interface Props {
    children: ReactNode;
    label: string;
    renderFallback: (error: Error | null) => ReactNode;
}

interface State {
    hasError: boolean;
    error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
    state: State = { hasError: false, error: null };

    static getDerivedStateFromError(error: Error): State {
        return { hasError: true, error };
    }

    componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
        console.error(`[${this.props.label}]`, error);
        console.error(`[${this.props.label}] Component stack:`, errorInfo.componentStack);
    }

    render() {
        return this.state.hasError ? this.props.renderFallback(this.state.error) : this.props.children;
    }
}
