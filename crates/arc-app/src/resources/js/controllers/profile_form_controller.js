import { Controller } from "@hotwired/stimulus";

/**
 * Profile form controller
 * Handles profile form submission with fetch and displays notifications
 */
export default class extends Controller {
    static targets = ["name", "email", "submitButton", "indicator"];
    static values = {
        waiting: { type: Boolean, default: false },
    };

    /**
     * Handle form submission
     * @param {Event} event
     */
    async handleSubmit(event) {
        event.preventDefault();

        this.waitingValue = true;
        this.showIndicator();
        this.disableSubmitButton();

        const formData = new FormData(this.element);
        const urlEncodedData = new URLSearchParams(formData).toString();

        try {
            const response = await fetch(this.element.action, {
                method: "POST",
                headers: {
                    "Content-Type": "application/x-www-form-urlencoded",
                },
                body: urlEncodedData,
            });

            const data = await response.json();

            if (response.ok) {
                // Update form values from response
                if (data.data) {
                    if (this.hasNameTarget && data.data.name) {
                        this.nameTarget.value = data.data.name;
                    }
                    if (this.hasEmailTarget && data.data.email) {
                        this.emailTarget.value = data.data.email;
                    }
                }
                this.notify("Profile updated successfully", "success");
            } else {
                this.notify("Failed to update profile", "error");
                if (data.errors) {
                    console.error(Object.values(data.errors));
                }
            }
        } catch (e) {
            this.notify("Failed to update profile", "error");
            console.error("Error submitting form:", e);
        } finally {
            this.waitingValue = false;
            this.hideIndicator();
            this.enableSubmitButton();
        }
    }

    /**
     * Show loading indicator
     */
    showIndicator() {
        if (this.hasIndicatorTarget) {
            this.indicatorTarget.hidden = false;
        }
    }

    /**
     * Hide loading indicator
     */
    hideIndicator() {
        if (this.hasIndicatorTarget) {
            this.indicatorTarget.hidden = true;
        }
    }

    /**
     * Disable submit button
     */
    disableSubmitButton() {
        if (this.hasSubmitButtonTarget) {
            this.submitButtonTarget.disabled = true;
        }
    }

    /**
     * Enable submit button
     */
    enableSubmitButton() {
        if (this.hasSubmitButtonTarget) {
            this.submitButtonTarget.disabled = false;
        }
    }

    /**
     * Dispatch notify event
     * @param {string} message
     * @param {string} type
     */
    notify(message, type) {
        window.dispatchEvent(
            new CustomEvent("notify", {
                detail: { message, type },
            })
        );
    }
}
