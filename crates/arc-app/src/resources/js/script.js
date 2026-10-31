import * as Turbo from "@hotwired/turbo";
import { Application } from "@hotwired/stimulus";
import Toastify from "toastify-js";
import "toastify-js/src/toastify.css";

// Initialize Stimulus application
const application = Application.start();

// Import controllers
import NotificationController from "./controllers/notification_controller";
import SessionNotificationController from "./controllers/session_notification_controller";
import ProfileFormController from "./controllers/profile_form_controller";
import PasswordFormController from "./controllers/password_form_controller";

// Register controllers
application.register("notification", NotificationController);
application.register("session-notification", SessionNotificationController);
application.register("profile-form", ProfileFormController);
application.register("password-form", PasswordFormController);

// Make Toastify available globally for notifications
window.Toastify = Toastify;

// Configure Turbo
Turbo.setProgressBarDelay(100);
