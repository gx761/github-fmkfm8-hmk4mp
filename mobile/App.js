import { StatusBar } from 'expo-status-bar';
import { useState } from 'react';
import {
  Pressable,
  SafeAreaView,
  ScrollView,
  StyleSheet,
  Text,
  View,
} from 'react-native';

const AGENTS = [
  { id: 'a1', name: 'Refactor auth module', status: 'running' },
  { id: 'a2', name: 'Write integration tests', status: 'queued' },
  { id: 'a3', name: 'Fix flaky CI job', status: 'done' },
];

const STATUS_COLOR = {
  running: '#2f80ed',
  queued: '#8a8f98',
  done: '#27ae60',
};

export default function App() {
  const [agents] = useState(AGENTS);

  return (
    <SafeAreaView style={styles.safe}>
      <StatusBar style="auto" />
      <ScrollView contentContainerStyle={styles.container}>
        <Text style={styles.title}>Multica Mobile</Text>
        <Text style={styles.subtitle}>
          Manage your coding agents on the go.
        </Text>

        <View style={styles.card}>
          <Text style={styles.cardHeader}>Active agents</Text>
          {agents.map((agent) => (
            <View key={agent.id} style={styles.row}>
              <View
                style={[
                  styles.dot,
                  { backgroundColor: STATUS_COLOR[agent.status] },
                ]}
              />
              <Text style={styles.rowName}>{agent.name}</Text>
              <Text style={styles.rowStatus}>{agent.status}</Text>
            </View>
          ))}
        </View>

        <Pressable style={styles.button}>
          <Text style={styles.buttonText}>Assign new task</Text>
        </Pressable>

        <Text style={styles.footer}>
          Starter scaffold — wire up the Multica API to make it live.
        </Text>
      </ScrollView>
    </SafeAreaView>
  );
}

const styles = StyleSheet.create({
  safe: {
    flex: 1,
    backgroundColor: '#0d1117',
  },
  container: {
    padding: 24,
    paddingTop: 48,
  },
  title: {
    color: '#ffffff',
    fontSize: 28,
    fontWeight: '700',
  },
  subtitle: {
    color: '#8a8f98',
    fontSize: 15,
    marginTop: 6,
    marginBottom: 24,
  },
  card: {
    backgroundColor: '#161b22',
    borderRadius: 14,
    padding: 16,
    borderWidth: 1,
    borderColor: '#21262d',
  },
  cardHeader: {
    color: '#c9d1d9',
    fontSize: 13,
    textTransform: 'uppercase',
    letterSpacing: 1,
    marginBottom: 12,
  },
  row: {
    flexDirection: 'row',
    alignItems: 'center',
    paddingVertical: 10,
  },
  dot: {
    width: 10,
    height: 10,
    borderRadius: 5,
    marginRight: 12,
  },
  rowName: {
    color: '#ffffff',
    fontSize: 15,
    flex: 1,
  },
  rowStatus: {
    color: '#8a8f98',
    fontSize: 13,
  },
  button: {
    backgroundColor: '#2f80ed',
    borderRadius: 12,
    paddingVertical: 14,
    alignItems: 'center',
    marginTop: 20,
  },
  buttonText: {
    color: '#ffffff',
    fontSize: 16,
    fontWeight: '600',
  },
  footer: {
    color: '#6e7681',
    fontSize: 12,
    textAlign: 'center',
    marginTop: 28,
  },
});
