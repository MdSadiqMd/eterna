FROM nginx:alpine
COPY nginx.conf          /etc/nginx/nginx.conf
COPY default.conf.tmpl   /etc/nginx/default.conf.tmpl
COPY entrypoint.sh       /entrypoint.sh
RUN  chmod +x /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
CMD ["nginx", "-g", "daemon off;"]
